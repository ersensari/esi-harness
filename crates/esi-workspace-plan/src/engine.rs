use crate::model::*;
use sha2::{Digest, Sha256};
use std::path::Path;

// ---------------------------------------------------------------------------
// FSM transition table
// ---------------------------------------------------------------------------

pub fn is_plan_transition_allowed(from: WorkspacePlanStatus, to: WorkspacePlanStatus) -> bool {
    matches!(
        (from, to),
        (
            WorkspacePlanStatus::Discovery,
            WorkspacePlanStatus::Planning
        ) | (WorkspacePlanStatus::Planning, WorkspacePlanStatus::Approved)
            | (WorkspacePlanStatus::Approved, WorkspacePlanStatus::Revising)
            | (WorkspacePlanStatus::Revising, WorkspacePlanStatus::Approved)
    )
}

// ---------------------------------------------------------------------------
// Content hash
// ---------------------------------------------------------------------------

fn compute_content_hash(plan: &WorkspacePlan) -> String {
    compute_snapshot_hash(&plan.content_snapshot())
}

pub(crate) fn compute_snapshot_hash(plan: &PlanContentSnapshot) -> String {
    if !plan.task_contracts.is_empty() {
        let mut content = serde_json::json!({
            "title": plan.title, "description": plan.description,
            "architecture_notes": plan.architecture_notes, "requirements": plan.requirements,
            "tasks": plan.tasks.iter().map(|task| serde_json::json!({
                "id": task.id, "title": task.title, "description": task.description
            })).collect::<Vec<_>>(),
            "task_contracts": plan.task_contracts, "innovation_discovery": plan.innovation_discovery,
        });
        content.sort_all_objects();
        let mut hasher = Sha256::new();
        hasher.update(b"esi-workspace-plan-contract-v1\0");
        hasher.update(serde_json::to_vec(&content).expect("typed JSON content"));
        return hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
    }
    let mut hasher = Sha256::new();
    hasher.update(plan.title.as_bytes());
    hasher.update(b"\x00");
    hasher.update(plan.description.as_bytes());
    hasher.update(b"\x00");
    hasher.update(plan.architecture_notes.as_bytes());
    hasher.update(b"\x00");
    for requirement in &plan.requirements {
        hasher.update(requirement.id.as_bytes());
        hasher.update(b"\x01");
        hasher.update(requirement.description.as_bytes());
        hasher.update(b"\x01");
        for criterion in &requirement.acceptance_criteria {
            hasher.update(criterion.as_bytes());
            hasher.update(b"\x02");
        }
        hasher.update(format!("{:?}", requirement.priority).as_bytes());
        hasher.update(b"\x00");
    }
    for task in &plan.tasks {
        hasher.update(task.id.as_bytes());
        hasher.update(b"\x01");
        hasher.update(task.title.as_bytes());
        hasher.update(b"\x01");
        hasher.update(task.description.as_bytes());
        hasher.update(b"\x00");
    }
    if let Some(innovation) = &plan.innovation_discovery {
        hasher.update(innovation.brief.as_bytes());
        hasher.update(b"\x00");
        for finding in &innovation.research_findings {
            hasher.update(finding.as_bytes());
            hasher.update(b"\x01");
        }
        for candidate in &innovation.candidates {
            hasher.update(candidate.as_bytes());
            hasher.update(b"\x01");
        }
        hasher.update(innovation.selected_rationale.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Return the current time as an RFC 3339 string using `SystemTime`.
/// This avoids requiring chrono's `clock` feature which is not enabled
/// in the workspace dependency configuration.
fn now_rfc3339() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Convert UNIX seconds → calendar fields (UTC). Handles years 1970–9999.
    const SECS_PER_DAY: u64 = 86_400;
    let days = secs / SECS_PER_DAY;
    let day_secs = secs % SECS_PER_DAY;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    // Civil date from days since 1970-01-01 (Howard Hinnant algorithm).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}+00:00")
}

// ---------------------------------------------------------------------------
// WorkspacePlan implementation
// ---------------------------------------------------------------------------

impl WorkspacePlan {
    /// Create a new workspace plan in `Discovery` status.
    pub fn new(
        workspace_path: impl AsRef<Path>,
        title: impl Into<String>,
    ) -> Result<Self, WorkspacePlanError> {
        let workspace_path = workspace_path.as_ref();
        let title = title.into();
        if title.trim().is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "workspace plan title must be non-empty".to_string(),
            ));
        }
        let canonical = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let workspace_id = {
            let mut hasher = Sha256::new();
            hasher.update(canonical.to_string_lossy().as_bytes());
            hasher
                .finalize()
                .iter()
                .take(8)
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };
        let now = now_rfc3339();
        let mut plan = Self {
            schema_version: SCHEMA_VERSION,
            storage_revision: 0,
            persisted_snapshot: None,
            workspace_id,
            canonical_path: canonical,
            status: WorkspacePlanStatus::Discovery,
            title,
            description: String::new(),
            requirements: Vec::new(),
            architecture_notes: String::new(),
            tasks: Vec::new(),
            task_contracts: Default::default(),
            innovation_discovery: None,
            created_at: now.clone(),
            updated_at: now,
            approval: None,
            approval_history: Vec::new(),
            revision_count: 0,
            events: Vec::new(),
            memory_sync: None,
        };
        plan.emit(PlanEventKind::Created);
        Ok(plan)
    }

    // -- Accessors ----------------------------------------------------------

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub fn status(&self) -> WorkspacePlanStatus {
        self.status
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    pub fn architecture_notes(&self) -> &str {
        &self.architecture_notes
    }

    pub fn tasks(&self) -> &[PlannedTask] {
        &self.tasks
    }

    pub fn task_contracts(&self) -> &std::collections::BTreeMap<String, TaskContract> {
        &self.task_contracts
    }

    pub fn task_execution_order(&self) -> Result<Vec<String>, WorkspacePlanError> {
        crate::task_contract::validate(&self.requirements, &self.tasks, &self.task_contracts)
    }

    pub fn set_task_contracts(
        &mut self,
        contracts: std::collections::BTreeMap<String, TaskContract>,
    ) -> Result<(), WorkspacePlanError> {
        crate::task_contract::validate(&self.requirements, &self.tasks, &contracts)?;
        if self.task_contracts == contracts {
            return Ok(());
        }
        self.task_contracts = contracts;
        self.touch();
        self.emit(PlanEventKind::PlanContentUpdated);
        self.invalidate_if_approved("task contracts updated")
    }

    pub fn innovation_discovery(&self) -> Option<&InnovationDiscovery> {
        self.innovation_discovery.as_ref()
    }

    pub fn approval(&self) -> Option<&PlanApproval> {
        self.approval.as_ref()
    }

    pub fn revision_count(&self) -> u32 {
        self.revision_count
    }

    pub fn storage_revision(&self) -> u64 {
        self.storage_revision
    }

    pub fn events(&self) -> &[PlanEvent] {
        &self.events
    }

    /// The last recorded Wiki bounded-memory capture attempt, if any.
    pub fn memory_sync(&self) -> Option<&MemorySyncRecord> {
        self.memory_sync.as_ref()
    }

    /// Returns `true` when the current content hash already has a
    /// [`MemorySyncOutcome::Synced`] record, so a caller can skip a
    /// redundant Wiki upsert for an unchanged approved plan.
    pub fn memory_already_synced(&self) -> bool {
        let current_hash = self.content_hash();
        matches!(
            &self.memory_sync,
            Some(record)
                if record.content_hash == current_hash
                    && record.outcome == MemorySyncOutcome::Synced
        )
    }

    /// Record the outcome of a Wiki bounded-memory capture attempt for the
    /// plan's current content. This never mutates the FSM status or the
    /// approval hash: capture is an outbox side effect of approval, not a
    /// precondition or part of the approved content.
    pub fn record_memory_sync(&mut self, outcome: MemorySyncOutcome) {
        self.memory_sync = Some(MemorySyncRecord {
            content_hash: self.content_hash(),
            attempted_at: now_rfc3339(),
            outcome,
        });
    }

    /// Compute the current content hash.
    pub fn content_hash(&self) -> String {
        compute_content_hash(self)
    }

    // -- Gate ---------------------------------------------------------------

    /// Returns `true` if the plan is approved and the content hash still
    /// matches the approval hash.
    pub fn is_implementation_allowed(&self) -> bool {
        self.status == WorkspacePlanStatus::Approved
            && self.task_execution_order().is_ok()
            && self
                .approval
                .as_ref()
                .is_some_and(|a| a.content_hash == self.content_hash())
            && self
                .approval_history
                .last()
                .is_some_and(|revision| revision.content.as_ref() == Some(&self.content_snapshot()))
    }

    /// Returns `Ok(())` if implementation is allowed, or a typed error
    /// describing why it is blocked. Use this as the deterministic gate
    /// before any mutating development stage.
    pub fn require_approved_plan(&self) -> Result<(), WorkspacePlanError> {
        self.task_execution_order()?;
        if self.status != WorkspacePlanStatus::Approved {
            return Err(WorkspacePlanError::ImplementationBlocked {
                status: self.status,
                reason: self.status.display_message().to_string(),
            });
        }
        match &self.approval {
            None => Err(WorkspacePlanError::ImplementationBlocked {
                status: self.status,
                reason: "plan has no recorded approval".to_string(),
            }),
            Some(approval) if approval.content_hash != self.content_hash() => {
                Err(WorkspacePlanError::ContentHashMismatch)
            }
            Some(_)
                if self.approval_history.last().is_some_and(|revision| {
                    revision.content.as_ref() == Some(&self.content_snapshot())
                }) =>
            {
                Ok(())
            }
            Some(_) => Err(WorkspacePlanError::ContentHashMismatch),
        }
    }

    // -- Mutations ----------------------------------------------------------

    pub fn replace_task_scope(
        &mut self,
        requirements: Vec<Requirement>,
        tasks: Vec<PlannedTask>,
        contracts: std::collections::BTreeMap<String, TaskContract>,
    ) -> Result<(), WorkspacePlanError> {
        if requirements.is_empty()
            || requirements
                .iter()
                .any(|requirement| requirement.description.trim().is_empty())
        {
            return Err(WorkspacePlanError::InvalidInput(
                "task scope needs non-empty requirements".into(),
            ));
        }
        crate::task_contract::validate(&requirements, &tasks, &contracts)?;
        if self.requirements == requirements
            && self.tasks == tasks
            && self.task_contracts == contracts
        {
            return Ok(());
        }
        self.requirements = requirements;
        self.tasks = tasks;
        self.task_contracts = contracts;
        self.touch();
        self.emit(PlanEventKind::RequirementsUpdated {
            count: self.requirements.len(),
        });
        self.emit(PlanEventKind::PlanContentUpdated);
        self.invalidate_if_approved("task scope updated")
    }

    /// Change the plan title. An approved plan moves to `Revising` when the
    /// title changes because the title is part of the approved content hash.
    pub fn set_title(&mut self, title: impl Into<String>) -> Result<(), WorkspacePlanError> {
        let title = title.into();
        if title.trim().is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "workspace plan title must be non-empty".to_string(),
            ));
        }
        if self.title == title {
            return Ok(());
        }
        self.title = title;
        self.touch();
        self.invalidate_if_approved("plan title updated")
    }

    /// Record requirements. Valid in `Discovery` or `Planning` status.
    /// If the plan was `Approved`, it transitions to `Revising`.
    pub fn set_requirements(
        &mut self,
        requirements: Vec<Requirement>,
    ) -> Result<(), WorkspacePlanError> {
        if requirements.is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "at least one requirement is needed".to_string(),
            ));
        }
        for req in &requirements {
            if req.id.trim().is_empty() || req.description.trim().is_empty() {
                return Err(WorkspacePlanError::InvalidInput(
                    "every requirement needs an id and description".to_string(),
                ));
            }
        }
        crate::task_contract::validate(&requirements, &self.tasks, &self.task_contracts)?;
        self.requirements = requirements;
        self.touch();
        self.emit(PlanEventKind::RequirementsUpdated {
            count: self.requirements.len(),
        });
        self.invalidate_if_approved("requirements updated")
    }

    /// Set the plan description and architecture notes. Transitions
    /// `Discovery` → `Planning` if requirements exist.
    pub fn set_plan_content(
        &mut self,
        description: impl Into<String>,
        architecture_notes: impl Into<String>,
        tasks: Vec<PlannedTask>,
    ) -> Result<(), WorkspacePlanError> {
        let description = description.into();
        let architecture_notes = architecture_notes.into();
        if description.trim().is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "plan description must be non-empty".to_string(),
            ));
        }
        crate::task_contract::validate(&self.requirements, &tasks, &self.task_contracts)?;
        self.description = description;
        self.architecture_notes = architecture_notes;
        self.tasks = tasks;
        self.touch();
        self.emit(PlanEventKind::PlanContentUpdated);

        // Auto-transition Discovery → Planning when we have requirements + plan
        if self.status == WorkspacePlanStatus::Discovery && !self.requirements.is_empty() {
            self.transition(WorkspacePlanStatus::Planning)?;
        }
        self.invalidate_if_approved("plan content updated")
    }

    /// Record Innovation discovery output for greenfield workspaces.
    pub fn set_innovation_discovery(
        &mut self,
        discovery: InnovationDiscovery,
    ) -> Result<(), WorkspacePlanError> {
        if discovery.brief.trim().is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "innovation discovery needs a brief".to_string(),
            ));
        }
        self.innovation_discovery = Some(discovery);
        self.touch();
        self.invalidate_if_approved("innovation discovery updated")
    }

    /// Approve the current plan. Only valid in `Planning` or `Revising`.
    pub fn approve(&mut self, approved_by: impl Into<String>) -> Result<(), WorkspacePlanError> {
        let approved_by = approved_by.into();
        if approved_by.trim().is_empty() {
            return Err(WorkspacePlanError::ApprovalMismatch(
                "approver identity must be non-empty".to_string(),
            ));
        }
        if !matches!(
            self.status,
            WorkspacePlanStatus::Planning | WorkspacePlanStatus::Revising
        ) {
            return Err(WorkspacePlanError::InvalidTransition {
                from: self.status,
                to: WorkspacePlanStatus::Approved,
            });
        }
        if self.requirements.is_empty() || self.description.trim().is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "plan must have requirements and a description before approval".to_string(),
            ));
        }
        self.task_execution_order()?;
        let hash = self.content_hash();
        let now = now_rfc3339();
        self.approval = Some(PlanApproval {
            approved_by: approved_by.clone(),
            approved_at: now,
            content_hash: hash,
        });
        self.approval_history.push(ApprovedPlanRevision {
            approval: self.approval.clone().expect("just recorded approval"),
            content: Some(self.content_snapshot()),
        });
        self.emit(PlanEventKind::Approved { approved_by });
        self.transition(WorkspacePlanStatus::Approved)
    }

    /// Explicitly request revision. Transitions `Approved` → `Revising`.
    pub fn request_revision(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<(), WorkspacePlanError> {
        let reason = reason.into();
        if self.status != WorkspacePlanStatus::Approved {
            return Err(WorkspacePlanError::InvalidTransition {
                from: self.status,
                to: WorkspacePlanStatus::Revising,
            });
        }
        self.revision_count += 1;
        self.emit(PlanEventKind::ApprovalInvalidated {
            reason: reason.clone(),
        });
        self.transition(WorkspacePlanStatus::Revising)
    }

    // -- Persistence --------------------------------------------------------

    /// Resolve the plan file path for a workspace directory.
    pub fn plan_path(workspace: impl AsRef<Path>) -> std::path::PathBuf {
        workspace.as_ref().join(PLAN_RELATIVE_PATH)
    }

    /// Check whether a workspace has any plan file (regardless of status).
    pub fn exists(workspace: impl AsRef<Path>) -> bool {
        Self::plan_path(workspace).is_file()
    }

    /// Load a workspace plan from its canonical location. Returns `None` if
    /// no plan file exists.
    pub fn load(workspace: impl AsRef<Path>) -> Result<Option<Self>, WorkspacePlanError> {
        let path = Self::plan_path(workspace);
        let Some((data, snapshot)) = crate::storage::read(&path)? else {
            return Ok(None);
        };
        let mut plan: Self = serde_json::from_slice(&data)?;
        let legacy = matches!(plan.schema_version, 1..=3);
        if legacy && !plan.approval_history.is_empty() {
            return Err(WorkspacePlanError::InvalidPersistedPlan);
        }
        if matches!(plan.schema_version, 1 | 2) && !plan.task_contracts.is_empty() {
            return Err(WorkspacePlanError::InvalidPersistedPlan);
        }
        if legacy {
            plan.schema_version = SCHEMA_VERSION;
        }
        if plan.schema_version != SCHEMA_VERSION {
            return Err(WorkspacePlanError::InvalidPersistedPlan);
        }
        plan.task_execution_order()?;
        if legacy {
            if let Some(approval) = &plan.approval {
                let content = (plan.status == WorkspacePlanStatus::Approved
                    && approval.content_hash == plan.content_hash())
                .then(|| plan.content_snapshot());
                plan.approval_history.push(ApprovedPlanRevision {
                    approval: approval.clone(),
                    content,
                });
            }
        }
        plan.validate_approval_history()?;
        plan.persisted_snapshot = Some(snapshot);
        Ok(Some(plan))
    }

    /// Save the workspace plan to its canonical location.
    pub fn save(&mut self, workspace: impl AsRef<Path>) -> Result<(), WorkspacePlanError> {
        let path = Self::plan_path(workspace);
        if self.schema_version != SCHEMA_VERSION {
            return Err(WorkspacePlanError::InvalidPersistedPlan);
        }
        self.task_execution_order()?;
        self.validate_approval_history()?;
        let mut next = self.clone();
        next.storage_revision = self
            .storage_revision
            .checked_add(1)
            .ok_or(crate::storage::PersistenceError::RevisionExhausted)?;
        let bytes = serde_json::to_vec_pretty(&next)?;
        next.persisted_snapshot = Some(crate::storage::commit(
            &path,
            self.persisted_snapshot.as_ref(),
            &bytes,
        )?);
        *self = next;
        Ok(())
    }

    // -- Internal -----------------------------------------------------------

    fn transition(&mut self, to: WorkspacePlanStatus) -> Result<(), WorkspacePlanError> {
        if !is_plan_transition_allowed(self.status, to) {
            return Err(WorkspacePlanError::InvalidTransition {
                from: self.status,
                to,
            });
        }
        let from = self.status;
        self.status = to;
        self.emit(PlanEventKind::StatusTransition { from, to });
        Ok(())
    }

    /// If the plan was approved and content changed, move to revising.
    fn invalidate_if_approved(&mut self, reason: &str) -> Result<(), WorkspacePlanError> {
        if self.status == WorkspacePlanStatus::Approved {
            if let Some(approval) = &self.approval {
                if approval.content_hash != self.content_hash()
                    || self.approval_history.last().is_some_and(|revision| {
                        revision
                            .content
                            .as_ref()
                            .is_some_and(|content| content != &self.content_snapshot())
                    })
                {
                    self.revision_count += 1;
                    self.emit(PlanEventKind::ApprovalInvalidated {
                        reason: reason.to_string(),
                    });
                    let from = self.status;
                    self.status = WorkspacePlanStatus::Revising;
                    self.emit(PlanEventKind::StatusTransition {
                        from,
                        to: WorkspacePlanStatus::Revising,
                    });
                }
            }
        }
        Ok(())
    }

    fn touch(&mut self) {
        self.updated_at = now_rfc3339();
    }

    fn emit(&mut self, event: PlanEventKind) {
        self.events.push(PlanEvent {
            sequence: self.events.len() as u64 + 1,
            timestamp: now_rfc3339(),
            status: self.status,
            event,
        });
    }
}

// ---------------------------------------------------------------------------
// Standalone gate for use by esi-development integration
// ---------------------------------------------------------------------------

/// Check the workspace plan gate for a given workspace directory. Returns
/// `Ok(())` if implementation is allowed, or a typed error if blocked.
///
/// If no plan file exists, returns `ImplementationBlocked` with `Discovery`
/// status, indicating that the planning flow must start.
pub fn check_workspace_plan_gate(workspace: impl AsRef<Path>) -> Result<(), WorkspacePlanError> {
    match WorkspacePlan::load(workspace)? {
        None => Err(WorkspacePlanError::ImplementationBlocked {
            status: WorkspacePlanStatus::Discovery,
            reason: "no workspace plan exists — start by gathering requirements".to_string(),
        }),
        Some(plan) => plan.require_approved_plan(),
    }
}
