use crate::{
    agents::{
        extension::PlatformExtensionContext,
        mcp_client::{Error, McpClientTrait},
        ToolCallContext,
    },
    session::SessionManager,
};
use async_trait::async_trait;
use esi_development::{
    ControllerService, DevelopmentState, StartPreparation, ValidationControl, ValidationPlan,
};
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, InitializeResult, JsonObject, ListToolsResult,
    ServerCapabilities, Tool, ToolAnnotations,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const EXTENSION_NAME: &str = "controller";
pub struct ControllerClient {
    sessions: Arc<SessionManager>,
    info: InitializeResult,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    task_id: String,
    request_id: Option<String>,
    validators: Option<Vec<esi_development::ValidationCommand>>,
}

impl ControllerClient {
    pub fn new(context: PlatformExtensionContext) -> anyhow::Result<Self> {
        Ok(Self { sessions: context.session_manager,
            info: InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
                .with_server_info(Implementation::new(EXTENSION_NAME, "1.0.0").with_title("ESI Controller"))
                .with_instructions("Use approved workspace task IDs. Start requires exact human execution approval. Reuse request IDs only for retries of the same operation. Status is evidence, not a completion claim. Validate runs approved commands in the owned worktree with network denied. Resume reconciles interruption and routes failed validation to repair; it never silently reruns a validator.") })
    }

    pub fn tools() -> Vec<Tool> {
        ["start", "status", "validate", "resume"].into_iter().map(|name| {
            let mut properties = json!({"task_id":{"type":"string","minLength":1,"maxLength":256}});
            let mut required = vec!["task_id"];
            if name != "status" {
                properties["request_id"] = json!({"type":"string","pattern":"^[A-Za-z0-9_-]{1,128}$"});
                required.push("request_id");
            }
            if name == "start" {
                properties["validators"] = json!({"type":"array","minItems":1,"items":{
                    "type":"object","additionalProperties":false,
                    "properties":{"id":{"type":"string"},"category":{"enum":["scope","syntax","static_policy","lint_type_build","targeted_tests","broad_tests"]},
                        "program":{"type":"string"},"arguments":{"type":"array","items":{"type":"string"}},"required":{"type":"boolean"}},
                    "required":["id","category","program","arguments","required"]}});
                required.push("validators");
            }
            Tool::new(name, format!("ESI controller {name}: persisted task-scoped operation, never model-supplied success."),
                json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).as_object().unwrap().clone())
                .annotate(ToolAnnotations::from_raw(None, Some(name == "status"), Some(false), Some(true), Some(false)))
        }).collect()
    }

    async fn execute(
        &self,
        context: &ToolCallContext,
        name: &str,
        input: Input,
        cancellation: CancellationToken,
    ) -> anyhow::Result<DevelopmentState> {
        use anyhow::{bail, ensure, Context};
        let root = crate::workspace_tool_authority::workspace(&self.sessions, context).await?;
        let service = ControllerService::new(
            &root,
            self.sessions.controller_data_dir(),
            context.session_id.clone(),
        )?;
        ensure!(
            matches!(name, "start" | "status" | "validate" | "resume"),
            "Unknown controller capability"
        );
        if name == "status" {
            ensure!(
                input.request_id.is_none() && input.validators.is_none(),
                "Status accepts only task_id"
            );
            return tokio::task::spawn_blocking(move || service.status(&input.task_id))
                .await?
                .map_err(Into::into);
        }
        crate::workspace_tool_authority::require_receipt(&root)?;
        ensure!(
            !cancellation.is_cancelled(),
            "Controller operation cancelled"
        );
        let request_id = input.request_id.context("request_id is required")?;
        if name == "start" {
            let interactive = context
                .tool_call_request_id
                .clone()
                .context("Start requires interactive human execution approval")?;
            let validation =
                ValidationPlan::new(input.validators.context("validators are required")?)?;
            let worker = service.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                worker.prepare_start(&input.task_id, &request_id, validation)
            })
            .await??;
            let prepared = match prepared {
                StartPreparation::Recorded(state) => return Ok(*state),
                StartPreparation::Ready(prepared) => prepared,
            };
            let presentation = json!({"run":prepared.state().run_id(), "worktree":prepared.inspection().record.identity,
                "snapshot":prepared.inspection().snapshot_id, "validation":prepared.state().plan(),
                "network":"denied", "environment":"isolated", "automatic_completion":false,
                "policy":"esi-contained-v1", "timeout_secs":300, "output_limit_bytes_per_stream":16384});
            let bridge = self.sessions.action_required();
            let result = tokio::select! {
                _ = cancellation.cancelled() => None,
                result = bridge.request_and_wait(context.session_id.clone(), interactive,
                    format!("Start this exact controller task and approve these validation commands?\n{}", serde_json::to_string_pretty(&presentation)?),
                    json!({"type":"object","properties":{"approve":{"type":"boolean","description":"Approve this task execution"}},"required":["approve"],"additionalProperties":false}), Duration::from_secs(300)) => Some(result),
            };
            let approved = matches!(result, Some(Ok(crate::action_required_manager::ElicitationOutcome::Accept(ref v))) if v == &json!({"approve":true}));
            crate::workspace_tool_authority::require_receipt(&root)?;
            let state = tokio::task::spawn_blocking(move || {
                service.complete_start(*prepared, approved.then_some("desktop-user"))
            })
            .await??;
            if !approved {
                bail!(
                    "Task execution was not approved; no validators or implementation were started"
                );
            }
            return Ok(state);
        }
        ensure!(
            input.validators.is_none(),
            "Only start accepts validation commands"
        );
        if name == "resume" {
            return tokio::task::spawn_blocking(move || {
                service.resume(&input.task_id, &request_id)
            })
            .await?
            .map_err(Into::into);
        }
        let control = ValidationControl::default();
        let worker_control = control.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            service.validate(&input.task_id, &request_id, &worker_control)
        });
        let state = tokio::select! {
            result = &mut worker => result??,
            _ = cancellation.cancelled() => { control.cancel(); worker.await?? }
        };
        crate::workspace_tool_authority::require_receipt(&root)?;
        Ok(state)
    }
}

#[async_trait]
impl McpClientTrait for ControllerClient {
    async fn list_tools(
        &self,
        _session_id: &str,
        _next_cursor: Option<String>,
        _cancellation_token: CancellationToken,
    ) -> Result<ListToolsResult, Error> {
        Ok(ListToolsResult {
            tools: Self::tools(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        context: &ToolCallContext,
        name: &str,
        arguments: Option<JsonObject>,
        cancellation: CancellationToken,
    ) -> Result<CallToolResult, Error> {
        let result =
            match serde_json::from_value::<Input>(Value::Object(arguments.unwrap_or_default())) {
                Ok(input) => self.execute(context, name, input, cancellation).await,
                Err(error) => Err(error.into()),
            };
        Ok(match result {
            Ok(state) => CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&state).expect("typed state serializes"),
            )]),
            Err(error) => {
                CallToolResult::error(vec![ContentBlock::text(format!("Controller: {error}"))])
            }
        })
    }
    fn get_info(&self) -> Option<&InitializeResult> {
        Some(&self.info)
    }
}
