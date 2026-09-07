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

// Serialize this platform extension's writes across chats while Wiki I/O is in
// flight. Cross-process filesystem locking remains M16-001's separate contract.
static PLAN_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
                    "implementation_allowed": plan.is_implementation_allowed()
                        && (!context.requires_workspace_receipt || crate::workspace_tool_authority::require_receipt(workspace).is_ok()),
                    "title": plan.title(),
                    "description": plan.description(),
                    "requirements": plan.requirements(),
                    "architecture_notes": plan.architecture_notes(),
                    "tasks": plan.tasks(),
                    "innovation": plan.innovation_discovery(),
                    "approval": plan.approval(),
                    "revision_count": plan.revision_count(),
                    "memory_sync": plan.memory_sync(),
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

    async fn approve(context: &ToolCallContext, retry_only: bool) -> CallToolResult {
        Self::approve_with_config(context, retry_only, crate::config::Config::global()).await
    }

    async fn approve_with_config(
        context: &ToolCallContext,
        retry_only: bool,
        config: &crate::config::Config,
    ) -> CallToolResult {
        let workspace = match Self::workspace(context) {
            Ok(workspace) => workspace,
            Err(error) => return Self::error(error),
        };
        let mut plan = match WorkspacePlan::load(workspace) {
            Ok(Some(plan)) => plan,
            Ok(None) => return Self::error("No workspace plan exists. Create a draft first."),
            Err(error) => return Self::error(format!("Could not read workspace plan: {error}")),
        };
        let identity = match WorkspacePlan::new(workspace, "Workspace identity") {
            Ok(identity) => identity,
            Err(_) => return Self::error("Could not resolve workspace identity."),
        };
        if plan.canonical_path() != identity.canonical_path()
            || plan.workspace_id() != identity.workspace_id()
        {
            return Self::error("Plan belongs to a different workspace; capture refused.");
        }
        if !plan.is_implementation_allowed() {
            if retry_only {
                return Self::error("Approve the current plan before retrying Wiki capture.");
            }
            if let Err(error) = plan.approve("desktop-user") {
                return Self::error(error.to_string());
            }
        }
        if plan.memory_already_synced() {
            return CallToolResult::success(vec![Self::visible_text(
                "Plan already approved and synced to Wiki; no records changed.",
            )]);
        }
        plan.record_memory_sync(esi_workspace_plan::MemorySyncOutcome::Pending {
            reason: "Approved plan capture pending".into(),
        });
        if let Err(error) = plan.save(workspace) {
            return Self::error(format!("Could not save workspace-plan approval: {error}"));
        }
        let outcome =
            crate::esi_wiki_memory::capture_approved_plan_with_config(config, &plan).await;
        // Never overwrite a revision made during network I/O. Reload preserves
        // unrelated task-status changes too. External atomic CAS is future work.
        let mut current = match WorkspacePlan::load(workspace) {
            Ok(Some(current))
                if current.content_hash() == plan.content_hash()
                    && current.workspace_id() == plan.workspace_id()
                    && current.is_implementation_allowed() =>
            {
                current
            }
            _ => {
                return Self::error(
                    "Plan changed during Wiki capture; inspect status before retrying.",
                )
            }
        };
        current.record_memory_sync(outcome.clone());
        if let Err(error) = current.save(workspace) {
            return Self::error(format!(
                "Approval retained but Wiki result could not be saved: {error}"
            ));
        }
        CallToolResult::success(vec![Self::visible_text(format!(
            "Workspace plan approved; implementation tools are enabled. Wiki memory: {}. Use retry_memory_sync after fixing a pending Wiki connection.",
            serde_json::to_string(&outcome).expect("memory outcome serializes")
        ))])
    }

    fn tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "retry_memory_sync",
                "Retry bounded Wiki capture of the already-approved plan. Never grants approval or sends chat history.",
                Self::schema::<NoParams>(),
            ).annotate(ToolAnnotations::from_raw(
                Some("Retry Approved Plan Wiki Capture".to_string()),
                Some(false), Some(false), Some(true), Some(true),
            )),
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
                "Request explicit human approval for the current workspace plan and bounded workspace-scoped Wiki capture of its visible scope, architecture, and selected rationale when Wiki is enabled.",
                Self::schema::<NoParams>(),
            )
            .annotate(ToolAnnotations::from_raw(
                Some("Approve Workspace Plan".to_string()),
                Some(false),
                Some(true),
                Some(false),
                Some(true),
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
        let _write_guard = if matches!(name, "save_draft" | "approve" | "retry_memory_sync") {
            Some(PLAN_WRITE_LOCK.lock().await)
        } else {
            None
        };
        Ok(match name {
            "status" => Self::status(context),
            "save_draft" => match Self::parse(arguments) {
                Ok(params) => Self::save_draft(context, params),
                Err(error) => Self::error(error),
            },
            "approve" | "retry_memory_sync" => match Self::parse::<NoParams>(arguments) {
                Ok(_) => Self::approve(context, name == "retry_memory_sync").await,
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
    async fn approved_capture_is_workspace_scoped_idempotent_and_revision_aware() {
        use wiremock::{matchers::path, Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(path("/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"result":{"stored":true}})),
            )
            .expect(6)
            .mount(&server)
            .await;
        let (_config_root, config) =
            crate::esi_wiki_memory::tests::fixture(&format!("{}/mcp", server.uri()), true);
        let workspace = TempDir::new().unwrap();
        let context = context(&workspace);
        assert_ne!(
            WorkspacePlanClient::save_draft(
                &context,
                WorkspacePlanClient::parse(Some(draft())).unwrap()
            )
            .is_error,
            Some(true)
        );
        assert_ne!(
            WorkspacePlanClient::approve_with_config(&context, false, &config)
                .await
                .is_error,
            Some(true)
        );
        let first = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        assert!(first.memory_already_synced());
        assert_ne!(
            WorkspacePlanClient::approve_with_config(&context, false, &config)
                .await
                .is_error,
            Some(true)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        let mut revision = draft();
        revision.insert(
            "description".into(),
            serde_json::json!("Revised visible scope"),
        );
        WorkspacePlanClient::save_draft(
            &context,
            WorkspacePlanClient::parse(Some(revision)).unwrap(),
        );
        assert_eq!(
            WorkspacePlanClient::approve_with_config(&context, true, &config)
                .await
                .is_error,
            Some(true)
        );
        assert_ne!(
            WorkspacePlanClient::approve_with_config(&context, false, &config)
                .await
                .is_error,
            Some(true)
        );
        let current = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        assert_eq!(first.workspace_id(), current.workspace_id());
        assert_ne!(first.content_hash(), current.content_hash());
        assert!(current.memory_already_synced());
        let requests = server.received_requests().await.unwrap();
        for (before, after) in requests[..3].iter().zip(&requests[3..]) {
            let before: serde_json::Value = serde_json::from_slice(&before.body).unwrap();
            let after: serde_json::Value = serde_json::from_slice(&after.body).unwrap();
            assert_eq!(
                before["params"]["arguments"]["key"],
                after["params"]["arguments"]["key"]
            );
            for payload in [&before, &after] {
                let args = &payload["params"]["arguments"];
                assert_eq!(args["scope"], "workspace");
                assert_eq!(args["workspace_id"], first.workspace_id());
                assert!(args.get("project_id").is_none());
                assert!(!args
                    .to_string()
                    .contains(workspace.path().to_str().unwrap()));
            }
        }
    }

    #[tokio::test]
    async fn wiki_failure_preserves_approval_and_retry_does_not_grant_approval() {
        let (_root, config) =
            crate::esi_wiki_memory::tests::fixture("http://127.0.0.1:9900/mcp", false);
        let workspace = TempDir::new().unwrap();
        let context = context(&workspace);
        WorkspacePlanClient::save_draft(
            &context,
            WorkspacePlanClient::parse(Some(draft())).unwrap(),
        );
        assert_eq!(
            WorkspacePlanClient::approve_with_config(&context, true, &config)
                .await
                .is_error,
            Some(true)
        );
        assert!(!WorkspacePlan::load(workspace.path())
            .unwrap()
            .unwrap()
            .is_implementation_allowed());
        assert_ne!(
            WorkspacePlanClient::approve_with_config(&context, false, &config)
                .await
                .is_error,
            Some(true)
        );
        let plan = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        assert!(plan.is_implementation_allowed());
        assert!(matches!(
            &plan.memory_sync().unwrap().outcome,
            esi_workspace_plan::MemorySyncOutcome::Pending { .. }
        ));
    }

    #[tokio::test]
    async fn private_or_oversized_plan_memory_is_rejected_before_egress() {
        use wiremock::MockServer;
        let server = MockServer::start().await;
        let (_root, config) =
            crate::esi_wiki_memory::tests::fixture(&format!("{}/mcp", server.uri()), true);
        for description in [
            "<think>private model reasoning</think>".to_string(),
            "x".repeat(9000),
            "wiki_session_private".into(),
        ] {
            let workspace = TempDir::new().unwrap();
            let context = context(&workspace);
            let mut input = draft();
            input.insert("description".into(), serde_json::json!(description));
            WorkspacePlanClient::save_draft(
                &context,
                WorkspacePlanClient::parse(Some(input)).unwrap(),
            );
            WorkspacePlanClient::approve_with_config(&context, false, &config).await;
            let plan = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
            assert!(plan.is_implementation_allowed());
            assert!(!plan.memory_already_synced());
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn copied_plan_cannot_publish_into_another_workspace() {
        let (_root, config) =
            crate::esi_wiki_memory::tests::fixture("http://127.0.0.1:9900/mcp", false);
        let workspace = TempDir::new().unwrap();
        let other = TempDir::new().unwrap();
        WorkspacePlanClient::save_draft(
            &context(&workspace),
            WorkspacePlanClient::parse(Some(draft())).unwrap(),
        );
        let plan = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        // Model a manually copied/tampered file; guarded save intentionally
        // refuses copying a loaded snapshot to another persistence target.
        std::fs::create_dir_all(other.path().join(".esi")).unwrap();
        std::fs::write(
            WorkspacePlan::plan_path(other.path()),
            serde_json::to_vec(&plan).unwrap(),
        )
        .unwrap();
        assert_eq!(
            WorkspacePlanClient::approve_with_config(&context(&other), false, &config)
                .await
                .is_error,
            Some(true)
        );
        let mut forged = serde_json::to_value(&plan).unwrap();
        forged["canonical_path"] = serde_json::json!(other.path().canonicalize().unwrap());
        std::fs::write(
            WorkspacePlan::plan_path(other.path()),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
        assert_eq!(
            WorkspacePlanClient::approve_with_config(&context(&other), false, &config)
                .await
                .is_error,
            Some(true)
        );
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
