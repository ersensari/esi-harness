use esi_workspace::WorktreeIdentity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

pub(crate) const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentStage {
    Brief,
    Plan,
    WorktreeReady,
    Implement,
    DeterministicValidate,
    Diagnose,
    Repair,
    HumanGate,
    Review,
    CompletionGate,
    Completed,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCategory {
    Scope,
    Syntax,
    StaticPolicy,
    LintTypeBuild,
    TargetedTests,
    BroadTests,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureCategory {
    Scope,
    Syntax,
    StaticPolicy,
    Build,
    Test,
    Environment,
    Review,
}

impl From<ValidationCategory> for FailureCategory {
    fn from(category: ValidationCategory) -> Self {
        match category {
            ValidationCategory::Scope => Self::Scope,
            ValidationCategory::Syntax => Self::Syntax,
            ValidationCategory::StaticPolicy => Self::StaticPolicy,
            ValidationCategory::LintTypeBuild => Self::Build,
            ValidationCategory::TargetedTests | ValidationCategory::BroadTests => Self::Test,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationCommand {
    pub id: String,
    pub category: ValidationCategory,
    pub program: String,
    pub arguments: Vec<String>,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationPlan {
    pub(crate) commands: Vec<ValidationCommand>,
}

impl ValidationPlan {
    pub fn new(commands: Vec<ValidationCommand>) -> Result<Self, DevelopmentError> {
        if commands.is_empty() {
            return Err(DevelopmentError::InvalidValidationPlan(
                "at least one validator is required".to_string(),
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut previous = None;
        for command in &commands {
            if command.id.trim().is_empty() || command.program.trim().is_empty() {
                return Err(DevelopmentError::InvalidValidationPlan(
                    "validator id and program must be non-empty".to_string(),
                ));
            }
            if !ids.insert(command.id.clone()) {
                return Err(DevelopmentError::InvalidValidationPlan(format!(
                    "duplicate validator id: {}",
                    command.id
                )));
            }
            if previous.is_some_and(|category| command.category < category) {
                return Err(DevelopmentError::InvalidValidationPlan(
                    "validators must follow deterministic category ordering".to_string(),
                ));
            }
            previous = Some(command.category);
        }
        if !commands.iter().any(|command| command.required) {
            return Err(DevelopmentError::InvalidValidationPlan(
                "at least one validator must be required".to_string(),
            ));
        }
        Ok(Self { commands })
    }

    pub fn commands(&self) -> &[ValidationCommand] {
        &self.commands
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplementationPlan {
    pub summary: String,
    pub validation: ValidationPlan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairPolicy {
    pub budgets: BTreeMap<FailureCategory, u32>,
    pub repeated_fingerprint_limit: u32,
}

impl Default for RepairPolicy {
    fn default() -> Self {
        Self {
            budgets: [
                (FailureCategory::Scope, 1),
                (FailureCategory::Syntax, 2),
                (FailureCategory::StaticPolicy, 1),
                (FailureCategory::Build, 2),
                (FailureCategory::Test, 2),
                (FailureCategory::Environment, 1),
                (FailureCategory::Review, 2),
            ]
            .into_iter()
            .collect(),
            repeated_fingerprint_limit: 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FailureFingerprint(pub(crate) String);

impl FailureFingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationOutcome {
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationEvidence {
    pub validator_id: String,
    pub category: ValidationCategory,
    pub command: Vec<String>,
    pub required: bool,
    pub outcome: ValidationOutcome,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub failure_fingerprint: Option<FailureFingerprint>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationRun {
    pub attempt: u32,
    pub snapshot_id: String,
    pub evidence: Vec<ValidationEvidence>,
    pub passed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingFailure {
    pub category: FailureCategory,
    pub fingerprint: FailureFingerprint,
    pub source_id: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HumanGateReason {
    RepeatedFailure {
        failure: PendingFailure,
        occurrences: u32,
    },
    RepairBudgetExhausted {
        failure: PendingFailure,
        attempts: u32,
        budget: u32,
    },
    AbandonRequested {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingHumanGate {
    pub gate_id: String,
    pub reason: HumanGateReason,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeBinding {
    pub identity: WorktreeIdentity,
    pub initial_head: String,
    pub initial_snapshot_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSnapshot {
    pub head: String,
    pub snapshot_id: String,
    pub dirty: bool,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeReadyApproval {
    pub run_id: String,
    pub repository_id: String,
    pub worktree_path: PathBuf,
    pub snapshot_id: String,
    pub approved_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepairApproval {
    pub run_id: String,
    pub gate_id: String,
    pub fingerprint: FailureFingerprint,
    pub approved_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionApproval {
    pub run_id: String,
    pub snapshot_id: String,
    pub approved_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbandonApproval {
    pub run_id: String,
    pub gate_id: String,
    pub approved_by: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    Approved { summary: String },
    Rejected { findings: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DevelopmentEventKind {
    RunStarted,
    BriefRecorded,
    PlanRecorded,
    StageTransition {
        from: DevelopmentStage,
        to: DevelopmentStage,
    },
    WorktreeBound {
        repository_id: String,
        worktree_path: PathBuf,
        snapshot_id: String,
    },
    WorktreeInspected {
        snapshot: WorktreeSnapshot,
    },
    ValidationFinished {
        run: ValidationRun,
    },
    FailureRouted {
        failure: PendingFailure,
        destination: DevelopmentStage,
    },
    ReviewRecorded {
        approved: bool,
        summary: String,
    },
    HumanApprovalRecorded {
        gate: String,
        approved_by: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevelopmentEvent {
    pub sequence: u64,
    pub stage: DevelopmentStage,
    pub event: DevelopmentEventKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevelopmentState {
    pub(crate) schema_version: u32,
    pub(crate) run_id: String,
    pub(crate) stage: DevelopmentStage,
    pub(crate) brief: Option<Brief>,
    pub(crate) plan: Option<ImplementationPlan>,
    pub(crate) worktree: Option<WorktreeBinding>,
    #[serde(default)]
    pub(crate) worktree_snapshot: Option<WorktreeSnapshot>,
    pub(crate) repair_policy: RepairPolicy,
    pub(crate) repair_attempts: BTreeMap<FailureCategory, u32>,
    pub(crate) repair_extensions: BTreeMap<FailureCategory, u32>,
    pub(crate) fingerprint_occurrences: BTreeMap<FailureFingerprint, u32>,
    pub(crate) validation_runs: Vec<ValidationRun>,
    pub(crate) validated_snapshot_id: Option<String>,
    pub(crate) pending_failure: Option<PendingFailure>,
    pub(crate) pending_human_gate: Option<PendingHumanGate>,
    pub(crate) events: Vec<DevelopmentEvent>,
}

#[derive(Debug, Error)]
pub enum DevelopmentError {
    #[error("invalid development transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: DevelopmentStage,
        to: DevelopmentStage,
    },
    #[error("invalid workflow input: {0}")]
    InvalidInput(String),
    #[error("invalid validation plan: {0}")]
    InvalidValidationPlan(String),
    #[error("worktree readiness requires an exact human approval")]
    WorktreeApprovalMismatch,
    #[error("worktree does not match the bound ESI-managed worktree")]
    WorktreeBindingMismatch,
    #[error("worktree readiness requires a clean ESI-managed worktree")]
    WorktreeNotReady,
    #[error("human approval does not match the pending gate")]
    ApprovalMismatch,
    #[error("validated worktree snapshot changed before review or completion")]
    ValidatedSnapshotChanged,
    #[error("workflow state schema or event history is invalid")]
    InvalidPersistedState,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl PartialEq for DevelopmentError {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl Eq for DevelopmentError {}
