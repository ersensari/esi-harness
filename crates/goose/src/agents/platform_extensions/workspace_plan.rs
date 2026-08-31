use std::path::Path;

use async_trait::async_trait;
use esi_workspace_plan::{
    InnovationDiscovery, PlannedTask, PlannedTaskStatus, Priority, Requirement, WorkspacePlan,
};
use rmcp::model::{
    Annotations, CallToolResult, ContentBlock, Implementation, InitializeResult, JsonObject,
    ListToolsResult, ServerCapabilities, TextContent, Tool, ToolAnnotations,
};
use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::agents::extension::PlatformExtensionContext;
use crate::agents::mcp_client::{Error, McpClientTrait};
use crate::agents::ToolCallContext;

pub static EXTENSION_NAME: &str = "workspaceplan";

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PlanPriority {
    Must,
    Should,
    Could,
    Wont,
}

impl From<PlanPriority> for Priority {
    fn from(value: PlanPriority) -> Self {
        match value {
            PlanPriority::Must => Self::Must,
            PlanPriority::Should => Self::Should,
            PlanPriority::Could => Self::Could,
            PlanPriority::Wont => Self::Wont,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RequirementInput {
    id: String,
    description: String,
    acceptance_criteria: Vec<String>,
    priority: PlanPriority,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TaskInput {
    id: String,
    title: String,
    description: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct InnovationInput {
    brief: String,
    research_findings: Vec<String>,
    candidates: Vec<String>,
    selected_rationale: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SaveDraftParams {
    title: String,
    description: String,
    architecture_notes: String,
    requirements: Vec<RequirementInput>,
    tasks: Vec<TaskInput>,
    innovation: Option<InnovationInput>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NoParams {}

pub struct WorkspacePlanClient {
    info: InitializeResult,
}

impl WorkspacePlanClient {
    pub fn new(_context: PlatformExtensionContext) -> anyhow::Result<Self> {
        let info = InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new(EXTENSION_NAME, "1.0.0").with_title("ESI Workspace Plan"),
            )
            .with_instructions(
                "Use status first. For an unplanned workspace, discuss requirements and \
                 Innovation options with the user, then save_draft. Call approve only after the \
                 user explicitly accepts the displayed plan; Desktop will ask the user to confirm.",
            );
        Ok(Self { info })
    }

    fn schema<T: JsonSchema>() -> JsonObject {
        serde_json::to_value(schema_for!(T))
            .expect("schema serialization should succeed")
            .as_object()
            .expect("schema should serialize to an object")
            .clone()
    }

    fn visible_text(text: impl Into<String>) -> ContentBlock {
        ContentBlock::Text(
            TextContent::new(text).with_annotations(Annotations::default().with_priority(0.0)),
        )
    }

    fn error(message: impl Into<String>) -> CallToolResult {
        CallToolResult::error(vec![Self::visible_text(message)])
    }

    fn workspace(context: &ToolCallContext) -> Result<&Path, String> {
        context
            .working_dir
            .as_deref()
            .ok_or_else(|| "This chat is not bound to a workspace folder.".to_string())
    }

    fn parse<T: serde::de::DeserializeOwned>(arguments: Option<JsonObject>) -> Result<T, String> {
        serde_json::from_value(serde_json::Value::Object(arguments.unwrap_or_default()))
            .map_err(|error| format!("Invalid workspace-plan input: {error}"))
    }

    fn status(context: &ToolCallContext) -> CallToolResult {
        let workspace = match Self::workspace(context) {
            Ok(workspace) => workspace,
            Err(error) => return Self::error(error),
        };
        match WorkspacePlan::load(workspace) {
            Ok(Some(plan)) => {
                let value = serde_json::json!({
                    "exists": true,
                    "path": WorkspacePlan::plan_path(workspace),
                    "status": plan.status(),
                    "implementation_allowed": plan.is_implementation_allowed(),
                    "title": plan.title(),
                    "description": plan.description(),
                    "requirements": plan.requirements(),
                    "architecture_notes": plan.architecture_notes(),
                    "tasks": plan.tasks(),
                    "innovation": plan.innovation_discovery(),
                    "approval": plan.approval(),
                    "revision_count": plan.revision_count(),
                });
                CallToolResult::success(vec![Self::visible_text(
                    serde_json::to_string_pretty(&value).expect("workspace plan status serializes"),
                )])
            }
            Ok(None) => CallToolResult::success(vec![Self::visible_text(
                serde_json::json!({
                    "exists": false,
                    "status": "discovery",
                    "implementation_allowed": false,
                    "next_action": "Discuss requirements and Innovation options, then call save_draft."
                })
                .to_string(),
            )]),
            Err(error) => Self::error(format!("Could not read workspace plan: {error}")),
        }
    }

    fn save_draft(context: &ToolCallContext, params: SaveDraftParams) -> CallToolResult {
        let workspace = match Self::workspace(context) {
            Ok(workspace) => workspace,
            Err(error) => return Self::error(error),
        };
        let mut plan = match WorkspacePlan::load(workspace) {
            Ok(Some(plan)) => plan,
            Ok(None) => match WorkspacePlan::new(workspace, params.title.clone()) {
                Ok(plan) => plan,
                Err(error) => return Self::error(error.to_string()),
            },
            Err(error) => return Self::error(format!("Could not read workspace plan: {error}")),
        };
        if let Err(error) = plan.set_title(params.title) {
            return Self::error(error.to_string());
        }
        let requirements = params
            .requirements
            .into_iter()
            .map(|requirement| Requirement {
                id: requirement.id,
                description: requirement.description,
                acceptance_criteria: requirement.acceptance_criteria,
                priority: requirement.priority.into(),
            })
            .collect();
        if let Err(error) = plan.set_requirements(requirements) {
            return Self::error(error.to_string());
        }
        if let Some(innovation) = params.innovation {
            if let Err(error) = plan.set_innovation_discovery(InnovationDiscovery {
                brief: innovation.brief,
                research_findings: innovation.research_findings,
                candidates: innovation.candidates,
                selected_rationale: innovation.selected_rationale,
            }) {
                return Self::error(error.to_string());
            }
        }
        let tasks = params
            .tasks
            .into_iter()
            .map(|task| PlannedTask {
                id: task.id,
                title: task.title,
                description: task.description,
                status: PlannedTaskStatus::Pending,
            })
            .collect();
        if let Err(error) =
            plan.set_plan_content(params.description, params.architecture_notes, tasks)
        {
            return Self::error(error.to_string());
        }
        if let Err(error) = plan.save(workspace) {
            return Self::error(format!("Could not save workspace plan: {error}"));
        }
        CallToolResult::success(vec![Self::visible_text(format!(
            "Workspace plan saved in {} state. Present it to the user and request explicit \
             approval before calling workspaceplan__approve.",
            plan.status().display_message()
        ))])
    }

    fn approve(context: &ToolCallContext) -> CallToolResult {
        let workspace = match Self::workspace(context) {
            Ok(workspace) => workspace,
            Err(error) => return Self::error(error),
        };
        let mut plan = match WorkspacePlan::load(workspace) {
            Ok(Some(plan)) => plan,
            Ok(None) => return Self::error("No workspace plan exists. Create a draft first."),
            Err(error) => return Self::error(format!("Could not read workspace plan: {error}")),
        };
        if let Err(error) = plan.approve("desktop-user") {
            return Self::error(error.to_string());
        }
        if let Err(error) = plan.save(workspace) {
            return Self::error(format!("Could not save workspace-plan approval: {error}"));
        }
        CallToolResult::success(vec![Self::visible_text(
            "Workspace plan approved by the Desktop user. Implementation tools are now enabled.",
        )])
    }

    fn tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "status",
                "Read the durable plan and implementation authorization for this workspace.",
                Self::schema::<NoParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Workspace Plan Status".to_string()),
                Some(true),
                Some(false),
                Some(true),
                Some(false),
            )),
            Tool::new(
                "save_draft",
                "Create or revise the durable workspace plan after requirements and Innovation discovery.",
                Self::schema::<SaveDraftParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Save Workspace Plan Draft".to_string()),
                Some(false),
                Some(true),
                Some(false),
                Some(false),
            )),
            Tool::new(
                "approve",
                "Request explicit human approval for the current workspace plan.",
                Self::schema::<NoParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Approve Workspace Plan".to_string()),
                Some(false),
                Some(true),
                Some(false),
                Some(false),
            )),
        ]
    }
}

#[async_trait]
impl McpClientTrait for WorkspacePlanClient {
    async fn list_tools(
        &self,
        _session_id: &str,
        _next_cursor: Option<String>,
        _cancellation_token: CancellationToken,
    ) -> Result<ListToolsResult, Error> {
        Ok(ListToolsResult {
            tools: Self::tools(),
            next_cursor: None,
            meta: None,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        context: &ToolCallContext,
        name: &str,
        arguments: Option<JsonObject>,
        _cancellation_token: CancellationToken,
    ) -> Result<CallToolResult, Error> {
        Ok(match name {
            "status" => Self::status(context),
            "save_draft" => match Self::parse(arguments) {
                Ok(params) => Self::save_draft(context, params),
                Err(error) => Self::error(error),
            },
            "approve" => match Self::parse::<NoParams>(arguments) {
                Ok(_) => Self::approve(context),
                Err(error) => Self::error(error),
            },
            _ => Self::error(format!("Unknown workspace-plan tool: {name}")),
        })
    }

    fn get_info(&self) -> Option<&InitializeResult> {
        Some(&self.info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::extension::PlatformExtensionContext;
    use crate::session::SessionManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn client(data: &TempDir) -> WorkspacePlanClient {
        WorkspacePlanClient::new(PlatformExtensionContext {
            extension_manager: None,
            session_manager: Arc::new(SessionManager::new(data.path().to_path_buf())),
            scheduler: None,
            session: None,
            use_login_shell_path: false,
        })
        .unwrap()
    }

    fn context(workspace: &TempDir) -> ToolCallContext {
        ToolCallContext::new(
            "session".to_string(),
            Some(workspace.path().to_path_buf()),
            Some("request".to_string()),
        )
    }

    fn draft() -> JsonObject {
        serde_json::json!({
            "title": "DWG BOH capacity",
            "description": "Analyze and visualize BOH storage capacity",
            "architecture_notes": "Drawing adapter plus vector UI",
            "requirements": [{
                "id": "REQ-001",
                "description": "Render storage areas",
                "acceptance_criteria": ["Capacity is visible"],
                "priority": "must"
            }],
            "tasks": [{
                "id": "TASK-001",
                "title": "Implement drawing adapter",
                "description": "Parse the approved interchange format"
            }],
            "innovation": {
                "brief": "Evaluate safe DWG ingestion",
                "research_findings": ["ezdxf reads DXF, not native DWG"],
                "candidates": ["ODA conversion", "LibreDWG conversion"],
                "selected_rationale": "Select after license and fidelity validation"
            }
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[tokio::test]
    async fn draft_status_and_human_approval_round_trip() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let client = client(&data);
        let context = context(&workspace);

        let draft_result = client
            .call_tool(
                &context,
                "save_draft",
                Some(draft()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_ne!(draft_result.is_error, Some(true));
        let plan = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        assert!(!plan.is_implementation_allowed());

        let approval_result = client
            .call_tool(
                &context,
                "approve",
                Some(JsonObject::new()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_ne!(approval_result.is_error, Some(true));
        let plan = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        assert!(plan.is_implementation_allowed());
        assert_eq!(plan.innovation_discovery().unwrap().candidates.len(), 2);
    }

    #[tokio::test]
    async fn revising_a_plan_revokes_approval() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let client = client(&data);
        let context = context(&workspace);
        client
            .call_tool(
                &context,
                "save_draft",
                Some(draft()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        client
            .call_tool(
                &context,
                "approve",
                Some(JsonObject::new()),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let mut revised = draft();
        revised.insert(
            "description".to_string(),
            serde_json::Value::String("Revised capacity workflow".to_string()),
        );
        client
            .call_tool(
                &context,
                "save_draft",
                Some(revised),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(!WorkspacePlan::load(workspace.path())
            .unwrap()
            .unwrap()
            .is_implementation_allowed());
    }
}
