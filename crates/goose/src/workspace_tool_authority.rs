//! Final authority for Studio-owned tools. Provider-native Codex/Claude
//! delegation is a separate, explicitly trusted operator boundary.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use esi_workspace_plan::WorkspacePlan;
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::action_required_manager::ElicitationOutcome;
use crate::agents::{mcp_client::McpClientTrait, ToolCallContext};
use crate::config::Config;
use crate::session::SessionManager;

mod sandbox;

// Serializes local mutations and approval invalidation across managers/chats.
// External trusted operators are not participants in this in-process lock.
static EXECUTION: Mutex<()> = Mutex::const_new(());

#[derive(Clone, Copy, Debug)]
pub(crate) enum Factory {
    Developer,
    WorkspacePlan,
}

impl Factory {
    pub(crate) fn registered(key: &str) -> Option<Self> {
        match key {
            "developer" => Some(Self::Developer),
            "workspaceplan" => Some(Self::WorkspacePlan),
            _ => None,
        }
    }
}

fn receipt_key(plan: &WorkspacePlan) -> String {
    format!("ESI_PLAN_RECEIPT_{}", plan.workspace_id())
}

fn receipt(plan: &WorkspacePlan) -> Value {
    json!({"workspace": plan.canonical_path(), "hash": plan.content_hash(),
        "revision": plan.revision_count(), "approval": plan.approval()})
}

pub(crate) fn require_receipt(workspace: &Path) -> Result<WorkspacePlan> {
    let plan = load_plan(workspace)?;
    plan.require_approved_plan()?;
    let saved: Value = Config::global()
        .get_stored_param(&receipt_key(&plan))
        .context("Approve this plan in Studio: no trusted human receipt exists")?;
    ensure!(
        saved == receipt(&plan),
        "Plan changed or approval was forged; request fresh human approval"
    );
    Ok(plan)
}

fn load_plan(workspace: &Path) -> Result<WorkspacePlan> {
    let plan =
        WorkspacePlan::load(workspace)?.context("Create and approve a workspace plan first")?;
    let identity = WorkspacePlan::new(workspace, "identity")?;
    ensure!(
        plan.workspace_id() == identity.workspace_id() && plan.canonical_path() == workspace,
        "Plan workspace identity mismatch"
    );
    Ok(plan)
}

async fn workspace(sessions: &SessionManager, ctx: &ToolCallContext) -> Result<PathBuf> {
    let session = sessions.get_session(&ctx.session_id, false).await?;
    let root = session.working_dir.canonicalize()?;
    ensure!(
        ctx.working_dir
            .as_ref()
            .context("Missing workspace binding")?
            .canonicalize()?
            == root,
        "Tool workspace differs from persisted session"
    );
    let config = PathBuf::from(Config::global().path());
    let config_parent = config
        .parent()
        .context("Missing config directory")?
        .canonicalize()?;
    ensure!(
        !config_parent.starts_with(&root),
        "Host configuration cannot be a tool workspace"
    );
    for path in [
        root.join(".esi"),
        root.join(".esi/workspace-plan.json"),
        root.join(".esi/workspace-plan.json.lock"),
    ] {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => {
                ensure!(
                    !meta.file_type().is_symlink(),
                    "Workspace plan controls cannot be symlinks"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    ensure!(
                        !meta.is_file() || meta.nlink() == 1,
                        "Workspace plan controls cannot be hard links"
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(root)
}

pub(crate) async fn check_permission(
    permissions: &crate::config::permission::PermissionManager,
    sessions: &Arc<SessionManager>,
    ctx: &ToolCallContext,
    extension: &str,
    tool: &str,
    arguments: &Option<JsonObject>,
    cancellation: &CancellationToken,
) -> Result<()> {
    use crate::config::permission::PermissionLevel;
    use crate::config::GooseMode;
    let names = [tool.to_string(), format!("{extension}__{tool}")];
    let levels = || {
        names
            .iter()
            .filter_map(|name| permissions.get_user_permission(name))
            .collect::<Vec<_>>()
    };
    ensure!(
        !levels().contains(&PermissionLevel::NeverAllow),
        "Tool is explicitly forbidden by user permission"
    );
    let mode = sessions
        .get_session(&ctx.session_id, false)
        .await?
        .goose_mode;
    let read_only = matches!(
        (extension, tool),
        ("developer", "tree") | ("workspaceplan", "status" | "revision_diff")
    );
    ensure!(
        mode != GooseMode::Chat || read_only,
        "Chat mode cannot execute mutations"
    );
    let ask = levels().contains(&PermissionLevel::AskBefore)
        || (!read_only
            && !levels().contains(&PermissionLevel::AlwaysAllow)
            && matches!(mode, GooseMode::Approve | GooseMode::SmartApprove));
    if ask && (extension, tool) != ("workspaceplan", "approve") {
        let request = ctx
            .tool_call_request_id
            .clone()
            .context("This tool requires interactive human permission")?;
        let bridge = sessions.action_required();
        let decision = tokio::select! {
            _ = cancellation.cancelled() => bail!("Tool permission cancelled"),
            decision = bridge.request_and_wait(ctx.session_id.clone(), request,
                format!("Allow this specific Studio tool call? {}\n{}", names[1], serde_json::to_string(arguments)?),
                json!({"type":"object","properties":{"allow":{"type":"boolean"}},"required":["allow"],"additionalProperties":false}),
                Duration::from_secs(300)) => decision?,
        };
        ensure!(
            matches!(decision, ElicitationOutcome::Accept(ref value) if value == &json!({"allow":true})),
            "Human declined tool permission"
        );
    }
    ensure!(
        !levels().contains(&PermissionLevel::NeverAllow),
        "Tool permission was revoked"
    );
    Ok(())
}

pub(crate) async fn execute(
    factory: Option<Factory>,
    sessions: &Arc<SessionManager>,
    ctx: &ToolCallContext,
    tool: &str,
    arguments: Option<JsonObject>,
    client: &dyn McpClientTrait,
    cancellation: CancellationToken,
) -> Result<CallToolResult> {
    let factory = factory
        .context("Untrusted tool registration; names and MCP annotations cannot grant authority")?;
    let root = workspace(sessions, ctx).await?;
    let mut ctx = ToolCallContext::new(
        ctx.session_id.clone(),
        Some(root.clone()),
        ctx.tool_call_request_id.clone(),
    );
    ctx.requires_workspace_receipt = true;
    if matches!(factory, Factory::WorkspacePlan) && tool == "approve" {
        ensure!(
            arguments.as_ref().is_none_or(|args| args.is_empty()),
            "Approval takes no agent-supplied proof"
        );
        let mut plan = load_plan(&root)?;
        let request = ctx.tool_call_request_id.clone().context(
            "Approval requires an interactive human request; direct app calls cannot approve",
        )?;
        let presented = serde_json::to_string_pretty(&json!({
            "workspace": root, "title": plan.title(), "description": plan.description(),
            "requirements": plan.requirements(), "architecture": plan.architecture_notes(),
            "tasks": plan.tasks(), "innovation": plan.innovation_discovery(),
            "task_contracts": plan.task_contracts(), "revision_diff": plan.revision_diff(),
            "revision": plan.storage_revision(), "hash": plan.content_hash()
        }))?;
        let human_bridge = sessions.action_required();
        let decision = tokio::select! {
            _ = cancellation.cancelled() => bail!("Approval cancelled"),
            decision = human_bridge.request_and_wait(
                ctx.session_id.clone(), request,
                format!("Approve implementation of this exact workspace plan?\n{presented}"),
                json!({"type":"object","properties":{"approve":{"type":"boolean","title":"Approve this plan"}},"required":["approve"],"additionalProperties":false}),
                Duration::from_secs(300),
            ) => decision?,
        };
        ensure!(
            matches!(decision, ElicitationOutcome::Accept(ref value) if value == &json!({"approve":true})),
            "Human did not approve this plan"
        );
        let _guard = EXECUTION.lock().await;
        ensure!(!cancellation.is_cancelled(), "Approval cancelled");
        // CAS save consumes the snapshot presented to the user; a concurrent
        // revision cannot be silently approved, even if content later returns.
        if plan.is_implementation_allowed() {
            plan.request_revision("Fresh explicit human approval")?;
        }
        plan.approve("desktop-user")?;
        plan.save(&root)?;
        Config::global().set_param(&receipt_key(&plan), receipt(&plan))?;
        return client
            .call_tool(&ctx, "retry_memory_sync", None, cancellation)
            .await
            .map_err(Into::into);
    }

    let _guard = tokio::select! {
        _ = cancellation.cancelled() => bail!("Tool cancelled"),
        guard = EXECUTION.lock() => guard,
    };
    ensure!(!cancellation.is_cancelled(), "Tool cancelled");
    match factory {
        Factory::WorkspacePlan => {
            match tool {
                "status" | "revision_diff" => {}
                "save_draft" => {
                    let identity = WorkspacePlan::new(&root, "identity")?;
                    Config::global().set_param(&receipt_key(&identity), Value::Null)?;
                }
                "create_template" => {}
                "retry_memory_sync" => {
                    require_receipt(&root)?;
                }
                _ => bail!("No registered workspace-plan capability for {tool}"),
            }
            client
                .call_tool(&ctx, tool, arguments, cancellation)
                .await
                .map_err(Into::into)
        }
        Factory::Developer => {
            ensure!(
                matches!(tool, "tree" | "shell" | "write" | "edit" | "read_image"),
                "No registered local capability for {tool}"
            );
            if matches!(tool, "shell" | "write" | "edit") {
                require_receipt(&root)?;
            }
            sandbox::execute(&root, tool, arguments.unwrap_or_default(), cancellation).await
        }
    }
}

#[cfg(test)]
mod tests;
