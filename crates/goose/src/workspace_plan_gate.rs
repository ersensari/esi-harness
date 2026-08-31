//! Code-enforced ESI workspace-plan gate for Desktop/Goose tool execution
//! (ADR-0010, TASK-M14-003).
//!
//! `esi-development`'s `approve_worktree()` already blocks the *local
//! development controller* workflow from reaching `implement` without an
//! approved workspace plan. That check only fires when a chat actually
//! drives the controller. Nothing previously stopped a chat from calling the
//! ordinary `shell`/`write`/`edit` tools directly, in any workspace, without
//! ever going through the controller. [`WorkspacePlanInspector`] closes that
//! gap: it runs as a [`ToolInspector`] for every tool call, resolves the
//! calling session's bound working directory, and denies shell execution and
//! file writes/edits outside the plan-management directory unless that
//! workspace has a currently approved plan.
//!
//! This is deterministic, code-level enforcement — not a skill/prompt
//! instruction the model could ignore. A revoked or hash-mismatched approval
//! (see `esi_workspace_plan::WorkspacePlan::require_approved_plan`) is
//! re-checked on every tool call, so a mid-chat plan revision immediately
//! revokes implementation permission again.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::GooseMode;
use crate::conversation::message::{Message, ToolRequest};
use crate::session::SessionManager;
use crate::tool_inspection::{categorize_tool, extract_string_arg, InspectionAction};
use crate::tool_inspection::{InspectionResult, ToolCategory, ToolInspector};

/// Tool-argument keys that carry a target file path, checked in priority order.
const PATH_ARG_KEYS: &[&str] = &["path", "file", "file_path"];
const WORKSPACE_PLAN_EXTENSION_PREFIX: &str = "workspaceplan__";
const WORKSPACE_PLAN_APPROVE_TOOL: &str = "workspaceplan__approve";

/// Deterministic tool inspector enforcing the ADR-0010 workspace plan gate
/// for direct Desktop/Goose tool execution.
pub struct WorkspacePlanInspector {
    session_manager: Arc<SessionManager>,
}

impl WorkspacePlanInspector {
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
    }

    /// Resolve the canonical working directory bound to `session_id`, the
    /// same directory used to locate `<workspace>/.esi/workspace-plan.json`.
    /// Returns `None` when the session cannot be resolved (for example, an
    /// ephemeral/test session with no persisted record); such requests are
    /// left to other inspectors rather than blocked, since there is no
    /// workspace to bind a plan to.
    async fn resolve_working_dir(&self, session_id: &str) -> Result<PathBuf> {
        if session_id.is_empty() {
            anyhow::bail!("the tool call is not bound to a persisted Desktop session");
        }
        let session = self
            .session_manager
            .get_session(session_id, false)
            .await
            .map_err(|error| anyhow::anyhow!("cannot resolve Desktop session: {error}"))?;
        Ok(canonicalize_best_effort(&session.working_dir))
    }
}

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Lexically normalize a path (resolve `.`/`..` components without touching
/// the filesystem). Used to test plan-management scope for paths that may
/// not exist yet (a not-yet-created `.esi/workspace-plan.json`).
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Returns `true` when `raw_path` (as given in a tool call's arguments,
/// resolved against `working_dir`) stays within `<working_dir>/.esi/`. Writes
/// scoped to that directory are bounded plan-management operations (creating
/// or revising the durable workspace plan itself) and are allowed even
/// without an approved plan, per ADR-0010's discovery/planning flow.
fn is_plan_management_path(working_dir: &Path, raw_path: &str) -> bool {
    let candidate = Path::new(raw_path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        working_dir.join(candidate)
    };
    let normalized = normalize_lexically(&joined);
    let plan_dir = normalize_lexically(&working_dir.join(".esi"));
    if std::fs::symlink_metadata(&plan_dir).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return false;
    }
    normalized == normalize_lexically(&working_dir.join(esi_workspace_plan::PLAN_RELATIVE_PATH))
}

/// Build the user-facing denial message. This is what the model receives in
/// the tool result and is expected to relay to the user; it must state the
/// exact next action so a vibe coder is not left guessing why a shell command
/// or file edit silently failed.
fn denial_message(tool_name: &str, error: &esi_workspace_plan::WorkspacePlanError) -> String {
    use esi_workspace_plan::{WorkspacePlanError, WorkspacePlanStatus};

    let next_action = match error {
        WorkspacePlanError::ImplementationBlocked {
            status: WorkspacePlanStatus::Discovery,
            ..
        } => {
            "Start the ESI workspace planning flow: gather the user's requirements, run \
             Innovation-style discovery for greenfield work, then draft the implementation plan."
        }

        WorkspacePlanError::ImplementationBlocked {
            status: WorkspacePlanStatus::Planning,
            ..
        } => {
            "Finish the implementation plan (description, architecture, tasks) and ask the \
              user to explicitly approve it before running this tool again."
        }
        WorkspacePlanError::ImplementationBlocked {
            status: WorkspacePlanStatus::Revising,
            ..
        } => {
            "The plan was revised after approval. Review the changes with the user and ask \
              them to re-approve the current plan before running this tool again."
        }
        WorkspacePlanError::ImplementationBlocked {
            status: WorkspacePlanStatus::Approved,
            ..
        } => {
            "The plan is missing a recorded approval record. Ask the user to approve the \
              current plan again before running this tool again."
        }
        WorkspacePlanError::ContentHashMismatch => {
            "The plan content no longer matches its last approval. Review the changes with the \
             user and ask them to re-approve the current plan before running this tool again."
        }
        _ => {
            "Ask the user to open or restart the ESI workspace planning flow before running \
              this tool again."
        }
    };

    format!(
        "🔒 ESI workspace plan gate blocked `{tool_name}`.\n\n\
         {error}\n\n\
         Next action: {next_action}\n\n\
         No shell commands, file edits, or package installation are permitted in this \
         workspace until the workspace plan is approved (`.esi/workspace-plan.json`)."
    )
}

fn unbound_session_denial_message(tool_name: &str, reason: &str) -> String {
    format!(
        "ESI workspace plan gate blocked `{tool_name}` because its Desktop session workspace \
         could not be verified: {reason}. Reopen the chat from the intended project folder, then \
         complete and approve `.esi/workspace-plan.json` before trying again."
    )
}

#[async_trait]
impl ToolInspector for WorkspacePlanInspector {
    fn name(&self) -> &'static str {
        "workspace_plan"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    async fn inspect(
        &self,
        session_id: &str,
        tool_requests: &[ToolRequest],
        _messages: &[Message],
        _goose_mode: GooseMode,
    ) -> Result<Vec<InspectionResult>> {
        // Every tool call in a single inspection batch shares one session,
        // so the bound working directory is resolved once and reused.
        let mut results = Vec::new();
        let requires_working_dir = tool_requests.iter().any(|request| {
            request
                .tool_call
                .as_ref()
                .map(|call| {
                    matches!(
                        categorize_tool(&call.name),
                        ToolCategory::Shell | ToolCategory::Write
                    ) || call.name == WORKSPACE_PLAN_APPROVE_TOOL
                })
                .unwrap_or(false)
        });
        if !requires_working_dir {
            return Ok(results);
        }
        let working_dir = match self.resolve_working_dir(session_id).await {
            Ok(working_dir) => working_dir,
            Err(error) => {
                for request in tool_requests {
                    let Ok(tool_call) = &request.tool_call else {
                        continue;
                    };
                    if matches!(
                        categorize_tool(&tool_call.name),
                        ToolCategory::Shell | ToolCategory::Write
                    ) || tool_call.name == WORKSPACE_PLAN_APPROVE_TOOL
                    {
                        results.push(InspectionResult {
                            tool_request_id: request.id.clone(),
                            action: InspectionAction::Deny,
                            reason: unbound_session_denial_message(
                                &tool_call.name,
                                &error.to_string(),
                            ),
                            confidence: 1.0,
                            inspector_name: self.name().to_string(),
                            finding_id: None,
                        });
                    }
                }
                return Ok(results);
            }
        };

        for request in tool_requests {
            let Ok(tool_call) = &request.tool_call else {
                continue;
            };
            let tool_name = tool_call.name.to_string();
            if tool_name == WORKSPACE_PLAN_APPROVE_TOOL {
                results.push(InspectionResult {
                    tool_request_id: request.id.clone(),
                    action: InspectionAction::RequireApproval(Some(
                        "Approve this workspace plan only if its requirements, architecture, \
                         Innovation decision, and task list match what you want ESI to build."
                            .to_string(),
                    )),
                    reason: "workspace plan approval requires an explicit human decision"
                        .to_string(),
                    confidence: 1.0,
                    inspector_name: self.name().to_string(),
                    finding_id: None,
                });
                continue;
            }
            if tool_name.starts_with(WORKSPACE_PLAN_EXTENSION_PREFIX) {
                continue;
            }
            let category = categorize_tool(&tool_name);
            if !matches!(category, ToolCategory::Shell | ToolCategory::Write) {
                // Read-only discovery and every other tool category are
                // bounded, non-mutating operations: never blocked by the
                // plan gate. Emit no result so other inspectors decide.
                continue;
            }

            if category == ToolCategory::Write {
                let arguments = tool_call
                    .arguments
                    .clone()
                    .map(serde_json::Map::from_iter)
                    .map(serde_json::Value::Object);
                if let Some(path_arg) = arguments
                    .as_ref()
                    .and_then(|v| extract_string_arg(v, PATH_ARG_KEYS))
                {
                    if is_plan_management_path(&working_dir, &path_arg) {
                        // Bounded plan-management write: allowed regardless
                        // of plan status.
                        continue;
                    }
                }
            }

            if let Err(error) = esi_workspace_plan::check_workspace_plan_gate(&working_dir) {
                results.push(InspectionResult {
                    tool_request_id: request.id.clone(),
                    action: InspectionAction::Deny,
                    reason: denial_message(&tool_name, &error),
                    confidence: 1.0,
                    inspector_name: self.name().to_string(),
                    finding_id: None,
                });
            }
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::message::ToolRequest;
    use crate::session::{SessionManager, SessionType};
    use esi_workspace_plan::{
        PlannedTask, PlannedTaskStatus, Priority, Requirement, WorkspacePlan,
    };
    use rmcp::model::CallToolRequestParams;
    use rmcp::object;
    use tempfile::TempDir;

    fn request(
        id: &str,
        name: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> ToolRequest {
        ToolRequest {
            id: id.to_string(),
            tool_call: Ok(CallToolRequestParams::new(name.to_string()).with_arguments(arguments)),
            metadata: None,
            tool_meta: None,
        }
    }

    async fn create_session(
        session_manager: &SessionManager,
        working_dir: &Path,
        name: &str,
    ) -> crate::session::Session {
        session_manager
            .create_session(
                working_dir.to_path_buf(),
                name.to_string(),
                SessionType::User,
                GooseMode::Auto,
            )
            .await
            .unwrap()
    }

    fn save_approved_plan(workspace: &Path) {
        let mut plan = WorkspacePlan::new(workspace, "DWG BOH capacity analysis").unwrap();
        plan.set_requirements(vec![Requirement {
            id: "REQ-001".to_string(),
            description: "Analyze BOH storage capacity from a store drawing".to_string(),
            acceptance_criteria: vec!["Storage areas render as vectors".to_string()],
            priority: Priority::Must,
        }])
        .unwrap();
        plan.set_plan_content(
            "Import an approved drawing format and visualize BOH storage",
            "Format adapter, geometry domain, capacity service, and vector UI",
            vec![PlannedTask {
                id: "TASK-001".to_string(),
                title: "Implement drawing adapter".to_string(),
                description: "Add the approved DWG conversion and parsing path".to_string(),
                status: PlannedTaskStatus::Pending,
            }],
        )
        .unwrap();
        plan.approve("workspace-owner").unwrap();
        plan.save(workspace).unwrap();
    }

    #[tokio::test]
    async fn dwg_first_prompt_cannot_create_environment_or_install_packages() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let session_manager = Arc::new(SessionManager::new(data.path().to_path_buf()));
        let session = create_session(&session_manager, workspace.path(), "DWG project").await;
        let inspector = WorkspacePlanInspector::new(session_manager);
        let requests = vec![
            request(
                "shell-1",
                "developer__shell",
                object!({ "command": "python3 -m venv .venv && pip install ezdxf" }),
            ),
            request(
                "write-1",
                "developer__write",
                object!({ "path": "app.py", "content": "import ezdxf" }),
            ),
            request("read-1", "developer__tree", object!({ "path": "." })),
        ];

        let results = inspector
            .inspect(&session.id, &requests, &[], GooseMode::Auto)
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .all(|result| result.action == InspectionAction::Deny));
        assert!(results
            .iter()
            .all(|result| result.reason.contains("Innovation-style discovery")));
        assert!(!workspace.path().join(".venv").exists());
        assert!(!workspace.path().join("app.py").exists());
    }

    #[tokio::test]
    async fn approved_plan_is_reused_by_chats_for_the_same_canonical_workspace() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        save_approved_plan(workspace.path());
        let session_manager = Arc::new(SessionManager::new(data.path().to_path_buf()));
        let first = create_session(&session_manager, workspace.path(), "First chat").await;
        let second_path = workspace.path().join(".");
        let second = create_session(&session_manager, &second_path, "Second chat").await;
        let inspector = WorkspacePlanInspector::new(session_manager);
        let shell = [request(
            "shell-1",
            "developer__shell",
            object!({ "command": "cargo test" }),
        )];

        assert!(inspector
            .inspect(&first.id, &shell, &[], GooseMode::Auto)
            .await
            .unwrap()
            .is_empty());
        assert!(inspector
            .inspect(&second.id, &shell, &[], GooseMode::Auto)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn plan_revision_immediately_revokes_tool_execution() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        save_approved_plan(workspace.path());
        let session_manager = Arc::new(SessionManager::new(data.path().to_path_buf()));
        let session = create_session(&session_manager, workspace.path(), "Revision chat").await;
        let inspector = WorkspacePlanInspector::new(session_manager);
        let shell = [request(
            "shell-1",
            "developer__shell",
            object!({ "command": "npm install" }),
        )];
        assert!(inspector
            .inspect(&session.id, &shell, &[], GooseMode::Auto)
            .await
            .unwrap()
            .is_empty());

        let mut plan = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
        plan.request_revision("Change the capacity calculation")
            .unwrap();
        plan.save(workspace.path()).unwrap();

        let results = inspector
            .inspect(&session.id, &shell, &[], GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].reason.contains("re-approve"));
    }

    #[tokio::test]
    async fn only_the_workspace_plan_file_is_writable_before_approval() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let session_manager = Arc::new(SessionManager::new(data.path().to_path_buf()));
        let session = create_session(&session_manager, workspace.path(), "Planning chat").await;
        let inspector = WorkspacePlanInspector::new(session_manager);
        let writes = vec![
            request(
                "plan",
                "developer__write",
                object!({ "path": ".esi/workspace-plan.json", "content": "{}" }),
            ),
            request(
                "escape",
                "developer__write",
                object!({ "path": ".esi/../app.py", "content": "" }),
            ),
            request(
                "other",
                "developer__write",
                object!({ "path": ".esi/notes.json", "content": "{}" }),
            ),
        ];

        let results = inspector
            .inspect(&session.id, &writes, &[], GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(
            results
                .iter()
                .map(|result| result.tool_request_id.as_str())
                .collect::<Vec<_>>(),
            ["escape", "other"]
        );
    }

    #[tokio::test]
    async fn unresolved_desktop_session_fails_closed_for_mutating_tools() {
        let data = TempDir::new().unwrap();
        let inspector =
            WorkspacePlanInspector::new(Arc::new(SessionManager::new(data.path().to_path_buf())));
        let requests = [request(
            "shell-1",
            "developer__shell",
            object!({ "command": "touch should-not-exist" }),
        )];

        let results = inspector
            .inspect("missing-session", &requests, &[], GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].reason.contains("could not be verified"));
    }

    #[tokio::test]
    async fn workspace_plan_approval_always_requires_a_human_confirmation() {
        let data = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        save_approved_plan(workspace.path());
        let session_manager = Arc::new(SessionManager::new(data.path().to_path_buf()));
        let session = create_session(&session_manager, workspace.path(), "Approval chat").await;
        let inspector = WorkspacePlanInspector::new(session_manager);
        let requests = [request(
            "approve-1",
            WORKSPACE_PLAN_APPROVE_TOOL,
            object!({}),
        )];

        let results = inspector
            .inspect(&session.id, &requests, &[], GooseMode::Auto)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(matches!(
            results[0].action,
            InspectionAction::RequireApproval(Some(_))
        ));
    }
}
