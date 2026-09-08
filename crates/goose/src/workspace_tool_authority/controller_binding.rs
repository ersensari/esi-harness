use super::*;
use esi_development::{
    ControllerService, DevelopmentStage, DevelopmentState, ManagedWorktreeLease, PreparedStart,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    session_id: String,
    task_id: String,
    run_id: String,
    source: PathBuf,
    data_root: PathBuf,
    plan_hash: String,
    plan_revision: u64,
}

fn data_root(sessions: &SessionManager) -> Result<PathBuf> {
    let path = sessions.controller_data_dir();
    Ok(path
        .parent()
        .context("missing session data root")?
        .canonicalize()?
        .join("esi-controller"))
}

fn key(scope: &str, value: &impl Serialize) -> Result<String> {
    let hash: String = Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("ESI_CONTROLLER_{scope}_{hash}"))
}
pub(super) fn session_key(sessions: &SessionManager, session: &str) -> Result<String> {
    key("SESSION", &(data_root(sessions)?, session))
}
fn workspace_key(sessions: &SessionManager, source: &Path) -> Result<String> {
    key("WORKSPACE", &(data_root(sessions)?, source))
}
fn optional<T: serde::de::DeserializeOwned>(key: &str) -> Result<Option<T>> {
    match Config::global().get_stored_param(key) {
        Ok(value) => Ok(Some(value)),
        Err(crate::config::ConfigError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn check_start(
    sessions: &SessionManager,
    session_id: &str,
    source: &Path,
    task_id: &str,
) -> Result<()> {
    if let Some(binding) = optional::<Binding>(&session_key(sessions, session_id)?)? {
        ensure!(
            binding.source == source
                && binding.data_root == data_root(sessions)?
                && binding.session_id == session_id,
            "Controller session workspace changed; use the original bound session"
        );
        if binding.task_id != task_id {
            let state = ControllerService::new(source, &binding.data_root, session_id.into())?
                .status(&binding.task_id)?;
            ensure!(
                matches!(
                    state.stage(),
                    DevelopmentStage::Completed | DevelopmentStage::Abandoned
                ),
                "Finish or abandon the active controller task before switching tasks"
            );
        }
    }
    Ok(())
}

pub(crate) async fn commit_start(
    sessions: &SessionManager,
    session_id: &str,
    source: &Path,
    service: ControllerService,
    prepared: PreparedStart,
    approved: bool,
) -> Result<DevelopmentState> {
    let _guard = EXECUTION.lock().await;
    ensure!(
        sessions
            .get_session(session_id, false)
            .await?
            .working_dir
            .canonicalize()?
            == source,
        "Session workspace changed while reviewing controller execution"
    );
    let plan = require_receipt(source)?;
    let operation = prepared
        .state()
        .operations()
        .values()
        .find(|r| r.request.kind == esi_development::OperationKind::Start)
        .context("Prepared controller start has no operation identity")?;
    let task_id = operation.request.task_id.clone();
    check_start(sessions, session_id, source, &task_id)?;
    if approved {
        // Persist deny-by-default workspace mode BEFORE activating the run.
        // Crash between state commit and binding cannot expose the main tree.
        Config::global().set_param(&workspace_key(sessions, source)?, true)?;
    }
    let state = tokio::task::spawn_blocking(move || {
        service.complete_start(prepared, approved.then_some("desktop-user"))
    })
    .await??;
    if approved {
        ensure!(
            state.stage() == DevelopmentStage::Implement,
            "Controller did not enter implementation"
        );
        Config::global().set_param(
            &session_key(sessions, session_id)?,
            Binding {
                session_id: session_id.into(),
                task_id,
                run_id: state.run_id().into(),
                source: source.into(),
                data_root: data_root(sessions)?,
                plan_hash: plan.content_hash(),
                plan_revision: plan.storage_revision(),
            },
        )?;
    }
    Ok(state)
}

pub(crate) async fn managed_worktree(
    sessions: &SessionManager,
    session_id: &str,
    source: &Path,
    tool: &str,
) -> Result<Option<ManagedWorktreeLease>> {
    let binding = optional::<Binding>(&session_key(sessions, session_id)?)?;
    let mode = optional::<bool>(&workspace_key(sessions, source)?)?.unwrap_or(false);
    let Some(binding) = binding else {
        ensure!(
            !mode || tool == "tree",
            "Workspace is controller-managed; start an approved task for this chat before writing"
        );
        return Ok(None);
    };
    ensure!(
        mode && binding.source == source
            && binding.session_id == session_id
            && binding.data_root == data_root(sessions)?,
        "Controller binding does not match this workspace/session"
    );
    let plan = require_receipt(source)?;
    ensure!(
        plan.content_hash() == binding.plan_hash
            && plan.storage_revision() == binding.plan_revision,
        "Controller source plan changed; old execution binding is invalid"
    );
    let service = ControllerService::new(source, &binding.data_root, session_id.into())?;
    Ok(Some(
        tokio::task::spawn_blocking(move || {
            service.lease_implementation(&binding.task_id, &binding.run_id)
        })
        .await??,
    ))
}

/// Reconcile a host-owned run after process interruption. A workspace mode
/// receipt plus a completed human-approved Start is required to restore routing.
pub(crate) async fn resume(
    sessions: &SessionManager,
    session_id: &str,
    source: &Path,
    task_id: String,
    request_id: String,
) -> Result<DevelopmentState> {
    let _guard = EXECUTION.lock().await;
    ensure!(
        sessions
            .get_session(session_id, false)
            .await?
            .working_dir
            .canonicalize()?
            == source,
        "Controller session workspace changed"
    );
    let plan = require_receipt(source)?;
    check_start(sessions, session_id, source, &task_id)?;
    let service = ControllerService::new(source, data_root(sessions)?, session_id.into())?;
    let task = task_id.clone();
    let state = tokio::task::spawn_blocking(move || service.resume(&task, &request_id)).await??;
    if state.worktree().is_some() {
        ensure!(
            optional::<bool>(&workspace_key(sessions, source)?)? == Some(true),
            "Missing host execution-mode receipt; cannot restore controller binding"
        );
        ensure!(state.operations().values().any(|r| r.request.kind == esi_development::OperationKind::Start
            && r.status == esi_development::OperationStatus::Completed)
            && state.events().iter().any(|e| matches!(&e.event,
                esi_development::DevelopmentEventKind::HumanApprovalRecorded { gate, .. } if gate == "worktree_ready")),
            "Missing completed human-approved start; cannot restore controller binding");
        Config::global().set_param(
            &session_key(sessions, session_id)?,
            Binding {
                session_id: session_id.into(),
                task_id,
                run_id: state.run_id().into(),
                source: source.into(),
                data_root: data_root(sessions)?,
                plan_hash: plan.content_hash(),
                plan_revision: plan.storage_revision(),
            },
        )?;
    }
    Ok(state)
}

pub(crate) async fn commit_gate(
    sessions: &SessionManager,
    session_id: &str,
    source: &Path,
    service: ControllerService,
    prepared: esi_development::PreparedGate,
    approved: bool,
) -> Result<DevelopmentState> {
    let _guard = EXECUTION.lock().await;
    ensure!(
        sessions
            .get_session(session_id, false)
            .await?
            .working_dir
            .canonicalize()?
            == source,
        "Controller session workspace changed during delivery review"
    );
    require_receipt(source)?;
    tokio::task::spawn_blocking(move || {
        service.complete_gate(prepared, approved.then_some("desktop-user"))
    })
    .await?
    .map_err(Into::into)
}
