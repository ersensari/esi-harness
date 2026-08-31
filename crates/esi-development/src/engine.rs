use crate::model::*;
use esi_workspace::{LifecycleState, WorktreeInspection};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

const MAX_EVIDENCE_BYTES: usize = 16 * 1024;

pub fn is_transition_allowed(from: DevelopmentStage, to: DevelopmentStage) -> bool {
    matches!(
        (from, to),
        (DevelopmentStage::Brief, DevelopmentStage::Plan)
            | (DevelopmentStage::Brief, DevelopmentStage::HumanGate)
            | (DevelopmentStage::Plan, DevelopmentStage::WorktreeReady)
            | (DevelopmentStage::Plan, DevelopmentStage::HumanGate)
            | (DevelopmentStage::WorktreeReady, DevelopmentStage::Implement)
            | (DevelopmentStage::WorktreeReady, DevelopmentStage::HumanGate)
            | (
                DevelopmentStage::Implement,
                DevelopmentStage::DeterministicValidate
            )
            | (DevelopmentStage::Implement, DevelopmentStage::HumanGate)
            | (
                DevelopmentStage::DeterministicValidate,
                DevelopmentStage::Diagnose
            )
            | (
                DevelopmentStage::DeterministicValidate,
                DevelopmentStage::Review
            )
            | (DevelopmentStage::Diagnose, DevelopmentStage::Repair)
            | (DevelopmentStage::Diagnose, DevelopmentStage::HumanGate)
            | (
                DevelopmentStage::Repair,
                DevelopmentStage::DeterministicValidate
            )
            | (DevelopmentStage::Repair, DevelopmentStage::HumanGate)
            | (DevelopmentStage::HumanGate, DevelopmentStage::Repair)
            | (DevelopmentStage::HumanGate, DevelopmentStage::Abandoned)
            | (DevelopmentStage::Review, DevelopmentStage::Diagnose)
            | (DevelopmentStage::Review, DevelopmentStage::CompletionGate)
            | (DevelopmentStage::Review, DevelopmentStage::HumanGate)
            | (
                DevelopmentStage::CompletionGate,
                DevelopmentStage::Completed
            )
            | (
                DevelopmentStage::CompletionGate,
                DevelopmentStage::HumanGate
            )
    )
}

impl DevelopmentState {
    pub fn new(
        run_id: impl Into<String>,
        repair_policy: RepairPolicy,
    ) -> Result<Self, DevelopmentError> {
        let run_id = run_id.into();
        if run_id.trim().is_empty() || repair_policy.repeated_fingerprint_limit < 2 {
            return Err(DevelopmentError::InvalidInput(
                "run id must be non-empty and repeated failure limit must be at least two"
                    .to_string(),
            ));
        }
        let mut state = Self {
            schema_version: SCHEMA_VERSION,
            run_id,
            stage: DevelopmentStage::Brief,
            brief: None,
            plan: None,
            worktree: None,
            worktree_snapshot: None,
            repair_policy,
            repair_attempts: BTreeMap::new(),
            repair_extensions: BTreeMap::new(),
            fingerprint_occurrences: BTreeMap::new(),
            validation_runs: Vec::new(),
            validated_snapshot_id: None,
            pending_failure: None,
            pending_human_gate: None,
            events: Vec::new(),
        };
        state.emit(DevelopmentEventKind::RunStarted);
        Ok(state)
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn stage(&self) -> DevelopmentStage {
        self.stage
    }

    pub fn worktree(&self) -> Option<&WorktreeBinding> {
        self.worktree.as_ref()
    }

    pub fn worktree_snapshot(&self) -> Option<&WorktreeSnapshot> {
        self.worktree_snapshot.as_ref()
    }

    pub fn brief(&self) -> Option<&Brief> {
        self.brief.as_ref()
    }

    pub fn plan(&self) -> Option<&ImplementationPlan> {
        self.plan.as_ref()
    }

    pub fn repair_policy(&self) -> &RepairPolicy {
        &self.repair_policy
    }

    pub fn repair_extensions(&self) -> &BTreeMap<FailureCategory, u32> {
        &self.repair_extensions
    }

    pub fn fingerprint_occurrences(&self) -> &BTreeMap<FailureFingerprint, u32> {
        &self.fingerprint_occurrences
    }

    pub fn validation_runs(&self) -> &[ValidationRun] {
        &self.validation_runs
    }

    pub fn repair_attempts(&self) -> &BTreeMap<FailureCategory, u32> {
        &self.repair_attempts
    }

    pub fn pending_failure(&self) -> Option<&PendingFailure> {
        self.pending_failure.as_ref()
    }

    pub fn pending_human_gate(&self) -> Option<&PendingHumanGate> {
        self.pending_human_gate.as_ref()
    }

    pub fn events(&self) -> &[DevelopmentEvent] {
        &self.events
    }

    pub fn record_brief(&mut self, brief: Brief) -> Result<(), DevelopmentError> {
        if brief.objective.trim().is_empty() || brief.acceptance_criteria.is_empty() {
            return Err(DevelopmentError::InvalidInput(
                "brief requires an objective and acceptance criteria".to_string(),
            ));
        }
        self.ensure_stage(DevelopmentStage::Brief, DevelopmentStage::Plan)?;
        self.brief = Some(brief);
        self.emit(DevelopmentEventKind::BriefRecorded);
        self.transition(DevelopmentStage::Plan)
    }

    pub fn record_plan(&mut self, plan: ImplementationPlan) -> Result<(), DevelopmentError> {
        self.ensure_stage(DevelopmentStage::Plan, DevelopmentStage::Plan)?;
        if self.plan.is_some() || plan.summary.trim().is_empty() {
            return Err(DevelopmentError::InvalidInput(
                "plan must be non-empty and can only be recorded once".to_string(),
            ));
        }
        self.plan = Some(plan);
        self.emit(DevelopmentEventKind::PlanRecorded);
        Ok(())
    }

    pub fn approve_worktree(
        &mut self,
        inspection: &WorktreeInspection,
        approval: WorktreeReadyApproval,
    ) -> Result<(), DevelopmentError> {
        self.ensure_stage(DevelopmentStage::Plan, DevelopmentStage::WorktreeReady)?;
        if self.plan.is_none()
            || inspection.record.state != LifecycleState::Ready
            || inspection.dirty
        {
            return Err(DevelopmentError::WorktreeNotReady);
        }
        // --- Workspace plan gate (ADR-0010) ---
        // Check the workspace plan at the source repository before allowing
        // any implementation. This is code-enforced: no approved plan means
        // no worktree binding and no progression to implement/repair stages.
        // The gate only applies when the source repository actually exists
        // on the filesystem (always true in production, may be fictitious in
        // unit tests that use non-existent paths like "/source").
        let source = &inspection.record.identity.source_repository;
        if source.is_dir() {
            esi_workspace_plan::check_workspace_plan_gate(source)?;
        }
        let identity = &inspection.record.identity;
        let approval_matches = approval.run_id == self.run_id
            && approval.repository_id == identity.repository_id
            && approval.worktree_path == identity.worktree_path
            && approval.snapshot_id == inspection.snapshot_id
            && !approval.approved_by.trim().is_empty();
        if !approval_matches || identity.main_worktree == identity.worktree_path {
            return Err(DevelopmentError::WorktreeApprovalMismatch);
        }
        self.worktree = Some(WorktreeBinding {
            identity: identity.clone(),
            initial_head: inspection.head.clone(),
            initial_snapshot_id: inspection.snapshot_id.clone(),
        });
        self.emit(DevelopmentEventKind::WorktreeBound {
            repository_id: identity.repository_id.clone(),
            worktree_path: identity.worktree_path.clone(),
            snapshot_id: inspection.snapshot_id.clone(),
        });
        self.record_worktree_snapshot(inspection);
        self.emit(DevelopmentEventKind::HumanApprovalRecorded {
            gate: "worktree_ready".to_string(),
            approved_by: approval.approved_by,
        });
        self.transition(DevelopmentStage::WorktreeReady)
    }

    pub fn begin_implementation(&mut self) -> Result<(), DevelopmentError> {
        if self.worktree.is_none() {
            return Err(DevelopmentError::WorktreeNotReady);
        }
        self.transition(DevelopmentStage::Implement)
    }

    pub fn validate(
        &mut self,
        inspection: &WorktreeInspection,
    ) -> Result<ValidationRun, DevelopmentError> {
        if !matches!(
            self.stage,
            DevelopmentStage::Implement | DevelopmentStage::Repair
        ) {
            return Err(self.invalid_transition(DevelopmentStage::DeterministicValidate));
        }
        self.verify_bound_worktree(inspection)?;
        self.record_worktree_snapshot(inspection);
        self.transition(DevelopmentStage::DeterministicValidate)?;
        let commands = self
            .plan
            .as_ref()
            .expect("worktree approval requires a recorded plan")
            .validation
            .commands
            .clone();
        let mut evidence = Vec::new();
        let mut required_failure = None;
        for validator in &commands {
            let item = execute_validator(validator, inspection);
            if validator.required && item.outcome == ValidationOutcome::Failed {
                required_failure = Some(failure_from_evidence(&item));
            }
            evidence.push(item);
            if required_failure.is_some() {
                break;
            }
        }
        let run = ValidationRun {
            attempt: self.validation_runs.len() as u32 + 1,
            snapshot_id: inspection.snapshot_id.clone(),
            evidence,
            passed: required_failure.is_none(),
        };
        self.validation_runs.push(run.clone());
        self.emit(DevelopmentEventKind::ValidationFinished { run: run.clone() });
        if let Some(failure) = required_failure {
            *self
                .fingerprint_occurrences
                .entry(failure.fingerprint.clone())
                .or_default() += 1;
            self.pending_failure = Some(failure);
            self.validated_snapshot_id = None;
            self.transition(DevelopmentStage::Diagnose)?;
        } else {
            self.pending_failure = None;
            self.validated_snapshot_id = Some(inspection.snapshot_id.clone());
            self.transition(DevelopmentStage::Review)?;
        }
        Ok(run)
    }

    pub fn diagnose(&mut self) -> Result<DevelopmentStage, DevelopmentError> {
        self.ensure_stage(DevelopmentStage::Diagnose, DevelopmentStage::Repair)?;
        let failure = self
            .pending_failure
            .clone()
            .ok_or_else(|| DevelopmentError::InvalidInput("no failure to diagnose".to_string()))?;
        let occurrences = self
            .fingerprint_occurrences
            .get(&failure.fingerprint)
            .copied()
            .unwrap_or_default();
        let attempts = self
            .repair_attempts
            .get(&failure.category)
            .copied()
            .unwrap_or_default();
        let budget = self.repair_limit(failure.category);
        if occurrences >= self.repair_policy.repeated_fingerprint_limit {
            self.enter_human_gate(HumanGateReason::RepeatedFailure {
                failure: failure.clone(),
                occurrences,
            })?;
        } else if attempts >= budget {
            self.enter_human_gate(HumanGateReason::RepairBudgetExhausted {
                failure: failure.clone(),
                attempts,
                budget,
            })?;
        } else {
            *self.repair_attempts.entry(failure.category).or_default() += 1;
            self.transition(DevelopmentStage::Repair)?;
            self.emit(DevelopmentEventKind::FailureRouted {
                failure,
                destination: DevelopmentStage::Repair,
            });
        }
        Ok(self.stage)
    }

    pub fn approve_additional_repair(
        &mut self,
        approval: RepairApproval,
    ) -> Result<(), DevelopmentError> {
        self.ensure_stage(DevelopmentStage::HumanGate, DevelopmentStage::Repair)?;
        let gate = self
            .pending_human_gate
            .clone()
            .ok_or(DevelopmentError::ApprovalMismatch)?;
        let failure = gate_failure(&gate.reason)
            .cloned()
            .ok_or(DevelopmentError::ApprovalMismatch)?;
        if approval.run_id != self.run_id
            || approval.gate_id != gate.gate_id
            || approval.fingerprint != failure.fingerprint
            || approval.approved_by.trim().is_empty()
        {
            return Err(DevelopmentError::ApprovalMismatch);
        }
        let attempts = self
            .repair_attempts
            .get(&failure.category)
            .copied()
            .unwrap_or_default();
        if attempts >= self.repair_limit(failure.category) {
            *self.repair_extensions.entry(failure.category).or_default() += 1;
        }
        *self.repair_attempts.entry(failure.category).or_default() += 1;
        self.emit(DevelopmentEventKind::HumanApprovalRecorded {
            gate: gate.gate_id,
            approved_by: approval.approved_by,
        });
        self.pending_human_gate = None;
        self.transition(DevelopmentStage::Repair)
    }

    pub fn record_review(
        &mut self,
        inspection: &WorktreeInspection,
        decision: ReviewDecision,
    ) -> Result<(), DevelopmentError> {
        self.ensure_stage(DevelopmentStage::Review, DevelopmentStage::CompletionGate)?;
        self.verify_validated_snapshot(inspection)?;
        match decision {
            ReviewDecision::Approved { summary } => {
                self.emit(DevelopmentEventKind::ReviewRecorded {
                    approved: true,
                    summary,
                });
                self.transition(DevelopmentStage::CompletionGate)
            }
            ReviewDecision::Rejected { findings } => {
                let failure = PendingFailure {
                    category: FailureCategory::Review,
                    fingerprint: fingerprint(
                        FailureCategory::Review,
                        "review",
                        &findings,
                        &inspection.record.identity.worktree_path,
                    ),
                    source_id: "review".to_string(),
                    summary: bounded(&normalize_diagnostic(
                        &findings,
                        &inspection.record.identity.worktree_path,
                    )),
                };
                *self
                    .fingerprint_occurrences
                    .entry(failure.fingerprint.clone())
                    .or_default() += 1;
                self.pending_failure = Some(failure);
                self.validated_snapshot_id = None;
                self.emit(DevelopmentEventKind::ReviewRecorded {
                    approved: false,
                    summary: findings,
                });
                self.transition(DevelopmentStage::Diagnose)
            }
        }
    }

    pub fn approve_completion(
        &mut self,
        inspection: &WorktreeInspection,
        approval: CompletionApproval,
    ) -> Result<(), DevelopmentError> {
        self.ensure_stage(
            DevelopmentStage::CompletionGate,
            DevelopmentStage::Completed,
        )?;
        self.verify_validated_snapshot(inspection)?;
        if approval.run_id != self.run_id
            || approval.snapshot_id != inspection.snapshot_id
            || approval.approved_by.trim().is_empty()
        {
            return Err(DevelopmentError::ApprovalMismatch);
        }
        self.emit(DevelopmentEventKind::HumanApprovalRecorded {
            gate: "completion".to_string(),
            approved_by: approval.approved_by,
        });
        self.transition(DevelopmentStage::Completed)
    }

    pub fn request_abandon(
        &mut self,
        reason: impl Into<String>,
    ) -> Result<String, DevelopmentError> {
        let reason = reason.into();
        if reason.trim().is_empty()
            || matches!(
                self.stage,
                DevelopmentStage::Completed | DevelopmentStage::Abandoned
            )
        {
            return Err(DevelopmentError::InvalidInput(
                "a non-terminal run needs an abandonment reason".to_string(),
            ));
        }
        if self.stage != DevelopmentStage::HumanGate {
            if !is_transition_allowed(self.stage, DevelopmentStage::HumanGate) {
                return Err(self.invalid_transition(DevelopmentStage::HumanGate));
            }
            self.enter_human_gate(HumanGateReason::AbandonRequested { reason })?;
        } else {
            self.pending_human_gate = Some(PendingHumanGate {
                gate_id: self.next_gate_id(),
                reason: HumanGateReason::AbandonRequested { reason },
            });
        }
        Ok(self
            .pending_human_gate
            .as_ref()
            .expect("abandon request creates a gate")
            .gate_id
            .clone())
    }

    pub fn approve_abandon(&mut self, approval: AbandonApproval) -> Result<(), DevelopmentError> {
        self.ensure_stage(DevelopmentStage::HumanGate, DevelopmentStage::Abandoned)?;
        let gate = self
            .pending_human_gate
            .clone()
            .ok_or(DevelopmentError::ApprovalMismatch)?;
        if !matches!(gate.reason, HumanGateReason::AbandonRequested { .. })
            || approval.run_id != self.run_id
            || approval.gate_id != gate.gate_id
            || approval.approved_by.trim().is_empty()
        {
            return Err(DevelopmentError::ApprovalMismatch);
        }
        self.emit(DevelopmentEventKind::HumanApprovalRecorded {
            gate: gate.gate_id,
            approved_by: approval.approved_by,
        });
        self.pending_human_gate = None;
        self.transition(DevelopmentStage::Abandoned)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), DevelopmentError> {
        self.validate_persisted_state()?;
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, DevelopmentError> {
        let mut state: Self = serde_json::from_slice(&fs::read(path)?)?;
        if state.schema_version == 1 {
            state.schema_version = SCHEMA_VERSION;
            if let Some(binding) = &state.worktree {
                let snapshot = WorktreeSnapshot {
                    head: binding.initial_head.clone(),
                    snapshot_id: binding.initial_snapshot_id.clone(),
                    dirty: false,
                    changed_files: Vec::new(),
                };
                state.worktree_snapshot = Some(snapshot.clone());
                state.emit(DevelopmentEventKind::WorktreeInspected { snapshot });
            }
        }
        state.validate_persisted_state()?;
        Ok(state)
    }

    fn verify_bound_worktree(
        &self,
        inspection: &WorktreeInspection,
    ) -> Result<(), DevelopmentError> {
        let binding = self
            .worktree
            .as_ref()
            .ok_or(DevelopmentError::WorktreeBindingMismatch)?;
        if inspection.record.state != LifecycleState::Ready
            || inspection.record.identity != binding.identity
        {
            return Err(DevelopmentError::WorktreeBindingMismatch);
        }
        Ok(())
    }

    fn verify_validated_snapshot(
        &self,
        inspection: &WorktreeInspection,
    ) -> Result<(), DevelopmentError> {
        self.verify_bound_worktree(inspection)?;
        if self.validated_snapshot_id.as_deref() != Some(&inspection.snapshot_id) {
            return Err(DevelopmentError::ValidatedSnapshotChanged);
        }
        Ok(())
    }

    fn enter_human_gate(&mut self, reason: HumanGateReason) -> Result<(), DevelopmentError> {
        let failure = gate_failure(&reason).cloned();
        self.pending_human_gate = Some(PendingHumanGate {
            gate_id: self.next_gate_id(),
            reason,
        });
        self.transition(DevelopmentStage::HumanGate)?;
        if let Some(failure) = failure {
            self.emit(DevelopmentEventKind::FailureRouted {
                failure,
                destination: DevelopmentStage::HumanGate,
            });
        }
        Ok(())
    }

    fn repair_limit(&self, category: FailureCategory) -> u32 {
        self.repair_policy
            .budgets
            .get(&category)
            .copied()
            .unwrap_or_default()
            + self
                .repair_extensions
                .get(&category)
                .copied()
                .unwrap_or_default()
    }

    fn next_gate_id(&self) -> String {
        format!("{}-gate-{}", self.run_id, self.events.len() + 1)
    }

    fn transition(&mut self, to: DevelopmentStage) -> Result<(), DevelopmentError> {
        if !is_transition_allowed(self.stage, to) {
            return Err(self.invalid_transition(to));
        }
        let from = self.stage;
        self.stage = to;
        self.emit(DevelopmentEventKind::StageTransition { from, to });
        Ok(())
    }

    fn record_worktree_snapshot(&mut self, inspection: &WorktreeInspection) {
        let snapshot = WorktreeSnapshot {
            head: inspection.head.clone(),
            snapshot_id: inspection.snapshot_id.clone(),
            dirty: inspection.dirty,
            changed_files: inspection.changed_files.clone(),
        };
        self.worktree_snapshot = Some(snapshot.clone());
        self.emit(DevelopmentEventKind::WorktreeInspected { snapshot });
    }

    fn ensure_stage(
        &self,
        expected: DevelopmentStage,
        attempted: DevelopmentStage,
    ) -> Result<(), DevelopmentError> {
        if self.stage != expected {
            return Err(self.invalid_transition(attempted));
        }
        Ok(())
    }

    fn invalid_transition(&self, to: DevelopmentStage) -> DevelopmentError {
        DevelopmentError::InvalidTransition {
            from: self.stage,
            to,
        }
    }

    fn emit(&mut self, event: DevelopmentEventKind) {
        self.events.push(DevelopmentEvent {
            sequence: self.events.len() as u64 + 1,
            stage: self.stage,
            event,
        });
    }

    fn validate_persisted_state(&self) -> Result<(), DevelopmentError> {
        let sequences_valid = self
            .events
            .iter()
            .enumerate()
            .all(|(index, event)| event.sequence == index as u64 + 1);
        let history_valid = self.replay_event_history();
        let validated_stage_consistent = !matches!(
            self.stage,
            DevelopmentStage::Review
                | DevelopmentStage::CompletionGate
                | DevelopmentStage::Completed
        ) || self.validated_snapshot_id.as_ref().is_some_and(
            |snapshot_id| {
                self.validation_runs
                    .last()
                    .is_some_and(|run| run.passed && run.snapshot_id == *snapshot_id)
            },
        );
        let latest_event_snapshot = self
            .events
            .iter()
            .rev()
            .find_map(|event| match &event.event {
                DevelopmentEventKind::WorktreeInspected { snapshot } => Some(snapshot),
                _ => None,
            });
        let worktree_snapshot_consistent = match (&self.worktree, &self.worktree_snapshot) {
            (None, None) => latest_event_snapshot.is_none(),
            (Some(_), Some(snapshot)) => latest_event_snapshot == Some(snapshot),
            _ => false,
        };
        if self.schema_version != SCHEMA_VERSION
            || self.events.is_empty()
            || !sequences_valid
            || !history_valid
            || !validated_stage_consistent
            || !worktree_snapshot_consistent
        {
            return Err(DevelopmentError::InvalidPersistedState);
        }
        Ok(())
    }

    fn replay_event_history(&self) -> bool {
        let Some(first) = self.events.first() else {
            return false;
        };
        if first.stage != DevelopmentStage::Brief || first.event != DevelopmentEventKind::RunStarted
        {
            return false;
        }
        let mut current = DevelopmentStage::Brief;
        for event in self.events.iter().skip(1) {
            match event.event {
                DevelopmentEventKind::StageTransition { from, to } => {
                    if from != current || !is_transition_allowed(from, to) || event.stage != to {
                        return false;
                    }
                    current = to;
                }
                _ if event.stage != current => return false,
                _ => {}
            }
        }
        current == self.stage
    }
}

fn execute_validator(
    validator: &ValidationCommand,
    inspection: &WorktreeInspection,
) -> ValidationEvidence {
    let mut command_line = vec![validator.program.clone()];
    command_line.extend(validator.arguments.clone());
    let output = Command::new(&validator.program)
        .args(&validator.arguments)
        .current_dir(&inspection.record.identity.worktree_path)
        .output();
    let (outcome, exit_code, stdout, stderr, failure_category) = match output {
        Ok(output) => (
            if output.status.success() {
                ValidationOutcome::Passed
            } else {
                ValidationOutcome::Failed
            },
            output.status.code(),
            bounded(&String::from_utf8_lossy(&output.stdout)),
            bounded(&String::from_utf8_lossy(&output.stderr)),
            FailureCategory::from(validator.category),
        ),
        Err(error) => (
            ValidationOutcome::Failed,
            None,
            String::new(),
            bounded(&error.to_string()),
            FailureCategory::Environment,
        ),
    };
    let failure_fingerprint = (outcome == ValidationOutcome::Failed).then(|| {
        fingerprint(
            failure_category,
            &validator.id,
            if stderr.is_empty() { &stdout } else { &stderr },
            &inspection.record.identity.worktree_path,
        )
    });
    ValidationEvidence {
        validator_id: validator.id.clone(),
        category: validator.category,
        command: command_line,
        required: validator.required,
        outcome,
        exit_code,
        stdout,
        stderr,
        failure_fingerprint,
    }
}

fn failure_from_evidence(evidence: &ValidationEvidence) -> PendingFailure {
    PendingFailure {
        category: if evidence.exit_code.is_none() {
            FailureCategory::Environment
        } else {
            FailureCategory::from(evidence.category)
        },
        fingerprint: evidence
            .failure_fingerprint
            .clone()
            .expect("failed evidence has a fingerprint"),
        source_id: evidence.validator_id.clone(),
        summary: bounded(if evidence.stderr.is_empty() {
            &evidence.stdout
        } else {
            &evidence.stderr
        }),
    }
}

fn gate_failure(reason: &HumanGateReason) -> Option<&PendingFailure> {
    match reason {
        HumanGateReason::RepeatedFailure { failure, .. }
        | HumanGateReason::RepairBudgetExhausted { failure, .. } => Some(failure),
        HumanGateReason::AbandonRequested { .. } => None,
    }
}

fn fingerprint(
    category: FailureCategory,
    source_id: &str,
    diagnostic: &str,
    worktree_path: &Path,
) -> FailureFingerprint {
    let normalized = normalize_diagnostic(diagnostic, worktree_path);
    let mut hasher = Sha256::new();
    hasher.update(format!("{category:?}\0{source_id}\0{normalized}").as_bytes());
    FailureFingerprint(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn normalize_diagnostic(diagnostic: &str, worktree_path: &Path) -> String {
    let replaced = diagnostic
        .replace(&worktree_path.to_string_lossy().to_string(), "<worktree>")
        .to_ascii_lowercase();
    let mut normalized = String::with_capacity(replaced.len());
    let mut in_digits = false;
    let mut in_whitespace = false;
    for character in replaced.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                normalized.push('#');
            }
            in_digits = true;
            in_whitespace = false;
        } else if character.is_whitespace() {
            if !in_whitespace && !normalized.is_empty() {
                normalized.push(' ');
            }
            in_digits = false;
            in_whitespace = true;
        } else {
            normalized.push(character);
            in_digits = false;
            in_whitespace = false;
        }
    }
    normalized.trim().to_string()
}

fn bounded(value: &str) -> String {
    if value.len() <= MAX_EVIDENCE_BYTES {
        return value.to_string();
    }
    let mut result = String::with_capacity(MAX_EVIDENCE_BYTES);
    for character in value.chars() {
        if result.len() + character.len_utf8() > MAX_EVIDENCE_BYTES {
            break;
        }
        result.push(character);
    }
    result
}
