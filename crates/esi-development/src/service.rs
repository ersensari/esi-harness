//! Local run lifecycle. Host adapters own human provenance and resolve paths;
//! no request here is an MCP authority grant and no run is completed from text.
use crate::*;
use esi_workspace::{SessionId, WorkspaceManager, WorktreeInspection};
use esi_workspace_plan::WorkspacePlan;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct ControllerService {
    source: PathBuf,
    state_root: PathBuf,
    workspaces: WorkspaceManager,
    session_id: String,
}

pub struct PreparedStart {
    state: DevelopmentState,
    inspection: WorktreeInspection,
    request_id: String,
    task_id: String,
    // Held across human review, so another process cannot reconcile a live start.
    _lease: OperationLease,
}

struct OperationLease(fs::File);
impl Drop for OperationLease {
    fn drop(&mut self) {
        // Explicit unlock also releases a transient fork-inherited descriptor
        // before CLOEXEC closes it in another command-spawning thread.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

impl PreparedStart {
    pub fn state(&self) -> &DevelopmentState {
        &self.state
    }
    pub fn inspection(&self) -> &WorktreeInspection {
        &self.inspection
    }
}

pub enum StartPreparation {
    Ready(Box<PreparedStart>),
    Recorded(Box<DevelopmentState>),
}

pub struct ManagedWorktreeLease {
    inspection: WorktreeInspection,
    _lease: OperationLease,
}
impl ManagedWorktreeLease {
    pub fn inspection(&self) -> &WorktreeInspection {
        &self.inspection
    }
}

impl ControllerService {
    /// Fresh host inspection, not the last persisted PASS label.
    pub fn evidence_current(&self, task_id: &str) -> Result<bool, DevelopmentError> {
        let state = self.status(task_id)?;
        self.require_current(&state)?;
        let Some(run) = state.validation_runs().last() else {
            return Ok(false);
        };
        let inspection = self
            .workspaces
            .inspect(&self.source, &SessionId::new(state.run_id())?)?;
        Ok(run.passed
            && run.snapshot_id == inspection.snapshot_id
            && state
                .worktree()
                .is_some_and(|w| w.identity == inspection.record.identity)
            && !state
                .operations()
                .values()
                .any(|r| r.status == OperationStatus::Started))
    }

    pub fn lease_implementation(
        &self,
        task_id: &str,
        expected_run_id: &str,
    ) -> Result<ManagedWorktreeLease, DevelopmentError> {
        let lease = self.lease(task_id)?;
        let state = self.status(task_id)?;
        self.require_current(&state)?;
        if state.run_id() != expected_run_id
            || !matches!(
                state.stage(),
                DevelopmentStage::Implement | DevelopmentStage::Repair
            )
            || state
                .operations()
                .values()
                .any(|operation| operation.status == OperationStatus::Started)
        {
            return Err(invalid("run is not the active implementation/repair task"));
        }
        let inspection = self
            .workspaces
            .inspect(&self.source, &SessionId::new(state.run_id())?)?;
        if state
            .worktree()
            .is_none_or(|binding| binding.identity != inspection.record.identity)
        {
            return Err(DevelopmentError::WorktreeBindingMismatch);
        }
        Ok(ManagedWorktreeLease {
            inspection,
            _lease: lease,
        })
    }

    pub fn new(
        source: impl AsRef<Path>,
        host_data: impl AsRef<Path>,
        session_id: String,
    ) -> Result<Self, DevelopmentError> {
        if session_id.trim().is_empty() || session_id.len() > 256 {
            return Err(invalid("invalid host session identity"));
        }
        let source = source.as_ref().canonicalize()?;
        fs::create_dir_all(host_data.as_ref())?;
        let host_data = host_data.as_ref().canonicalize()?;
        let state_root = host_data.join("runs").join(digest(&source)?);
        Ok(Self {
            source,
            state_root,
            session_id,
            workspaces: WorkspaceManager::new(
                host_data.join("ownership"),
                host_data.join("worktrees"),
            ),
        })
    }

    pub fn state_path(&self, task_id: &str) -> Result<PathBuf, DevelopmentError> {
        if task_id.trim().is_empty() || task_id.len() > 256 {
            return Err(invalid("invalid task identity"));
        }
        Ok(self.state_root.join(format!("{}.json", digest(&task_id)?)))
    }

    fn lease(&self, task_id: &str) -> Result<OperationLease, DevelopmentError> {
        let path = self.state_path(task_id)?.with_extension("operation.lock");
        fs::create_dir_all(&self.state_root)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(invalid("invalid controller lease file"));
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|_| invalid("controller operation is active in another caller"))?;
        Ok(OperationLease(file))
    }

    fn source_plan(&self) -> Result<WorkspacePlan, DevelopmentError> {
        let plan =
            WorkspacePlan::load(&self.source)?.ok_or_else(|| invalid("source plan is missing"))?;
        plan.require_approved_plan()?;
        if plan.canonical_path() != self.source {
            return Err(invalid("source plan identity mismatch"));
        }
        Ok(plan)
    }

    pub fn status(&self, task_id: &str) -> Result<DevelopmentState, DevelopmentError> {
        let state = DevelopmentState::load(self.state_path(task_id)?)?;
        if state
            .operations()
            .values()
            .any(|r| r.request.task_id != task_id || r.request.session_id != self.session_id)
        {
            return Err(invalid("run belongs to a different task or session"));
        }
        if state
            .worktree()
            .is_some_and(|w| w.identity.source_repository != self.source)
        {
            return Err(invalid("run belongs to another source workspace"));
        }
        Ok(state)
    }

    fn require_current(&self, state: &DevelopmentState) -> Result<WorkspacePlan, DevelopmentError> {
        let plan = self.source_plan()?;
        let policy = state
            .plan()
            .ok_or_else(|| invalid("run has no validation policy"))?;
        let policy_digest = policy_digest(&policy.validation)?;
        if state.operations().values().any(|r| {
            r.request.source_plan_hash != plan.content_hash()
                || r.request.source_revision != plan.storage_revision()
                || r.request.policy_digest != policy_digest
                || r.request.session_id != self.session_id
        }) {
            return Err(invalid(
                "source approval or validation policy changed; existing run cannot mutate",
            ));
        }
        Ok(plan)
    }

    fn request(
        &self,
        state: &DevelopmentState,
        task_id: &str,
        request_id: &str,
        kind: OperationKind,
        snapshot: String,
    ) -> Result<OperationRequest, DevelopmentError> {
        let plan = self.require_current(state)?;
        let request = OperationRequest {
            request_id: request_id.into(),
            kind,
            source_plan_hash: plan.content_hash(),
            source_revision: plan.storage_revision(),
            task_id: task_id.into(),
            session_id: self.session_id.clone(),
            policy_digest: policy_digest(
                &state
                    .plan()
                    .ok_or_else(|| invalid("missing policy"))?
                    .validation,
            )?,
            expected_snapshot: snapshot,
        };
        if !request.valid() {
            return Err(invalid("invalid operation request"));
        }
        if state
            .operations()
            .get(request_id)
            .is_some_and(|prior| prior.request.kind != kind)
        {
            return Err(invalid("request id reused for another operation"));
        }
        // Snapshot is host-observed, not a caller payload. A repeated request
        // returns its original result, even if files subsequently changed.
        Ok(if let Some(prior) = state.operations().get(request_id) {
            OperationRequest {
                expected_snapshot: prior.request.expected_snapshot.clone(),
                ..request
            }
        } else {
            request
        })
    }

    pub fn prepare_start(
        &self,
        task_id: &str,
        request_id: &str,
        validation: ValidationPlan,
    ) -> Result<StartPreparation, DevelopmentError> {
        let lease = self.lease(task_id)?;
        let plan = self.source_plan()?;
        let task = plan
            .tasks()
            .iter()
            .find(|t| t.id == task_id)
            .ok_or_else(|| invalid("task is not in the approved plan"))?;
        let validation = ValidationPlan::new(validation.commands().to_vec())?;
        if let Some(contract) = plan.task_contracts().get(task_id) {
            for dependency in &contract.depends_on {
                let other = self.status(dependency)?;
                self.require_current(&other)?;
                if other.stage() != DevelopmentStage::Completed {
                    return Err(invalid("dependency has no completed controller evidence"));
                }
            }
        }
        let path = self.state_path(task_id)?;
        let mut state = if path.exists() {
            let existing = self.status(task_id)?;
            if existing.plan().is_none_or(|p| p.validation != validation) {
                return Err(invalid("run validation commands changed"));
            }
            existing
        } else {
            let id = digest(&(self.source.clone(), task_id))?;
            let mut state = DevelopmentState::new(
                format!("esi-{}", id.chars().take(32).collect::<String>()),
                RepairPolicy::default(),
            )?;
            let criteria = plan
                .requirements()
                .iter()
                .flat_map(|r| r.acceptance_criteria.clone())
                .collect();
            state.record_brief(Brief {
                objective: task.description.clone(),
                acceptance_criteria: criteria,
            })?;
            state.record_plan(ImplementationPlan {
                summary: task.title.clone(),
                validation,
            })?;
            state
        };
        let request = self.request(
            &state,
            task_id,
            request_id,
            OperationKind::Start,
            "creation-intent".into(),
        )?;
        if matches!(
            state.reserve_operation(&path, request)?,
            OperationReservation::Recorded(_)
        ) {
            return Ok(StartPreparation::Recorded(Box::new(state)));
        }
        let inspection =
            self.workspaces
                .create(&self.source, SessionId::new(state.run_id())?, "HEAD")?;
        Ok(StartPreparation::Ready(Box::new(PreparedStart {
            state,
            inspection,
            request_id: request_id.into(),
            task_id: task_id.into(),
            _lease: lease,
        })))
    }

    /// Only host code which obtained an exact human decision may call this.
    /// PreparedStart is opaque and cannot be deserialized from model arguments.
    pub fn complete_start(
        &self,
        mut prepared: PreparedStart,
        approved_by: Option<&str>,
    ) -> Result<DevelopmentState, DevelopmentError> {
        self.require_current(&prepared.state)?;
        if let Some(approved_by) = approved_by {
            let current = self.workspaces.inspect(
                &self.source,
                &prepared.inspection.record.identity.session_id,
            )?;
            if current != prepared.inspection {
                return Err(DevelopmentError::ValidatedSnapshotChanged);
            }
            prepared.state.approve_worktree(
                &current,
                WorktreeReadyApproval {
                    run_id: prepared.state.run_id().into(),
                    repository_id: current.record.identity.repository_id.clone(),
                    worktree_path: current.record.identity.worktree_path.clone(),
                    snapshot_id: current.snapshot_id.clone(),
                    approved_by: approved_by.into(),
                },
            )?;
            prepared.state.begin_implementation()?;
        }
        prepared.state.finish_operation(
            self.state_path(&prepared.task_id)?,
            &prepared.request_id,
            if approved_by.is_some() {
                OperationStatus::Completed
            } else {
                OperationStatus::Failed
            },
        )?;
        Ok(prepared.state)
    }

    pub fn validate(
        &self,
        task_id: &str,
        request_id: &str,
        control: &ValidationControl,
    ) -> Result<DevelopmentState, DevelopmentError> {
        let defaults = ValidationControl::default();
        if control.timeout != defaults.timeout
            || control.output_limit_bytes != defaults.output_limit_bytes
        {
            return Err(invalid(
                "validation limits differ from the approved controller policy",
            ));
        }
        let _lease = self.lease(task_id)?;
        let mut state = self.status(task_id)?;
        self.require_current(&state)?;
        let id = SessionId::new(state.run_id())?;
        let inspection = self.workspaces.inspect(&self.source, &id)?;
        let request = self.request(
            &state,
            task_id,
            request_id,
            OperationKind::Validate,
            inspection.snapshot_id.clone(),
        )?;
        let path = self.state_path(task_id)?;
        if matches!(
            state.reserve_operation(&path, request)?,
            OperationReservation::Recorded(_)
        ) {
            return Ok(state);
        }
        let mut contained_control = control.clone();
        contained_control.contained = true;
        let result = state.validate_with_inspector(&inspection, &contained_control, || {
            self.workspaces
                .inspect(&self.source, &id)
                .map_err(Into::into)
        });
        if let Err(error) = result {
            state.finish_operation(&path, request_id, OperationStatus::Failed)?;
            return Err(error);
        }
        // Source may be changed while commands execute. Never publish evidence
        // into Review after its source approval was revoked/revised.
        if self.require_current(&state).is_err() {
            state.request_abandon("Source plan changed during validation")?;
            state.finish_operation(&path, request_id, OperationStatus::Interrupted)?;
            return Ok(state);
        }
        state.finish_operation(&path, request_id, OperationStatus::Completed)?;
        Ok(state)
    }

    pub fn resume(
        &self,
        task_id: &str,
        request_id: &str,
    ) -> Result<DevelopmentState, DevelopmentError> {
        let _lease = self.lease(task_id)?;
        let mut state = self.status(task_id)?;
        self.require_current(&state)?;
        let path = self.state_path(task_id)?;
        // Possession of the OS lease proves no participating caller is still
        // executing. Do not respawn an interrupted validator automatically.
        let interrupted: Vec<_> = state
            .operations()
            .iter()
            .filter(|(_, r)| r.status == OperationStatus::Started)
            .map(|(id, _)| id.clone())
            .collect();
        for id in interrupted {
            state.finish_operation(&path, &id, OperationStatus::Interrupted)?;
        }
        if state.worktree().is_some() {
            let inspection = self
                .workspaces
                .inspect(&self.source, &SessionId::new(state.run_id())?)?;
            if matches!(
                state.stage(),
                DevelopmentStage::Review | DevelopmentStage::CompletionGate
            ) && state
                .validation_runs()
                .last()
                .is_none_or(|r| r.snapshot_id != inspection.snapshot_id)
            {
                return Err(DevelopmentError::ValidatedSnapshotChanged);
            }
        }
        let request = self.request(
            &state,
            task_id,
            request_id,
            OperationKind::Resume,
            "resume".into(),
        )?;
        if matches!(
            state.reserve_operation(&path, request)?,
            OperationReservation::Recorded(_)
        ) {
            return Ok(state);
        }
        if state.stage() == DevelopmentStage::Diagnose {
            state.diagnose()?;
        }
        state.finish_operation(&path, request_id, OperationStatus::Completed)?;
        Ok(state)
    }
}

fn digest(value: &impl serde::Serialize) -> Result<String, DevelopmentError> {
    Ok(Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
fn policy_digest(validation: &ValidationPlan) -> Result<String, DevelopmentError> {
    digest(&("esi-contained-v1", 300_u64, 16_384_usize, validation))
}
fn invalid(message: &str) -> DevelopmentError {
    DevelopmentError::InvalidInput(message.into())
}
