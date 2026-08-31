use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Relative path from the workspace root to the plan file.
pub const PLAN_RELATIVE_PATH: &str = ".esi/workspace-plan.json";

// ---------------------------------------------------------------------------
// Status FSM
// ---------------------------------------------------------------------------

/// Status of a workspace plan. Transitions are deterministic:
///
/// ```text
/// (none) → Discovery → Planning → Approved ↔ Revising
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePlanStatus {
    /// Gathering requirements and performing Innovation-style discovery.
    Discovery,
    /// Authoring the implementation plan from gathered requirements.
    Planning,
    /// Plan has been explicitly approved by a human. Implementation allowed.
    Approved,
    /// Plan content changed after approval. Re-approval required.
    Revising,
}

impl WorkspacePlanStatus {
    /// Human-readable status message for non-developer users.
    pub fn display_message(&self) -> &'static str {
        match self {
            Self::Discovery => "🔍 Gathering requirements — tell ESI what you want to build",
            Self::Planning => "📋 Creating your plan — ESI is designing the implementation",
            Self::Approved => "✅ Plan approved — ready to build",
            Self::Revising => "✏️ Plan changed — re-approval needed before building",
        }
    }
}

// ---------------------------------------------------------------------------
// MoSCoW priority
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Must,
    Should,
    Could,
    Wont,
}

// ---------------------------------------------------------------------------
// Requirements
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    pub id: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub priority: Priority,
}

// ---------------------------------------------------------------------------
// Planned tasks
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannedTaskStatus {
    Pending,
    Active,
    Completed,
    Skipped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: PlannedTaskStatus,
}

// ---------------------------------------------------------------------------
// Innovation discovery output
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InnovationDiscovery {
    /// Free-text brief describing the innovation goal.
    pub brief: String,
    /// Research findings or references gathered during discovery.
    pub research_findings: Vec<String>,
    /// Candidate approaches considered.
    pub candidates: Vec<String>,
    /// The selected approach rationale.
    pub selected_rationale: String,
}

// ---------------------------------------------------------------------------
// Plan approval
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanApproval {
    pub approved_by: String,
    pub approved_at: String,
    /// SHA-256 of plan content fields at approval time.
    pub content_hash: String,
}

// ---------------------------------------------------------------------------
// Wiki bounded-memory capture outbox (TASK-POST-122)
// ---------------------------------------------------------------------------

/// Outcome of the most recent attempt to upsert bounded workspace-memory
/// records into ESI-Wiki for this plan's approval (or approved revision).
///
/// This is deliberately excluded from [`WorkspacePlan::content_hash`] so
/// recording a capture attempt never itself invalidates approval, and
/// capture failures never block or silently misreport plan approval itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum MemorySyncOutcome {
    /// All bounded records were upserted into Wiki successfully.
    Synced,
    /// Wiki capture did not complete (unreachable, unauthorized after
    /// renewal, or rejected). This is an explicit outbox state: a later
    /// approval/re-approval or explicit retry may complete it. Approval
    /// itself already succeeded and is not reverted by this outcome.
    Pending { reason: String },
}

/// A durable, non-hash-affecting record of the last Wiki memory-capture
/// attempt for this plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySyncRecord {
    /// Content hash of the plan at the time this capture was attempted.
    /// Used to detect whether a later approval changed content, so an
    /// unchanged already-synced plan is never re-upserted needlessly.
    pub content_hash: String,
    pub attempted_at: String,
    #[serde(flatten)]
    pub outcome: MemorySyncOutcome,
}

// ---------------------------------------------------------------------------
// Plan events
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanEventKind {
    Created,
    StatusTransition {
        from: WorkspacePlanStatus,
        to: WorkspacePlanStatus,
    },
    RequirementsUpdated {
        count: usize,
    },
    PlanContentUpdated,
    Approved {
        approved_by: String,
    },
    ApprovalInvalidated {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEvent {
    pub sequence: u64,
    pub timestamp: String,
    pub status: WorkspacePlanStatus,
    pub event: PlanEventKind,
}

// ---------------------------------------------------------------------------
// The workspace plan
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePlan {
    pub(crate) schema_version: u32,
    pub(crate) workspace_id: String,
    pub(crate) canonical_path: PathBuf,
    pub(crate) status: WorkspacePlanStatus,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) requirements: Vec<Requirement>,
    pub(crate) architecture_notes: String,
    pub(crate) tasks: Vec<PlannedTask>,
    pub(crate) innovation_discovery: Option<InnovationDiscovery>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) approval: Option<PlanApproval>,
    pub(crate) revision_count: u32,
    pub(crate) events: Vec<PlanEvent>,
    /// Outbox state for bounded Wiki memory capture (TASK-POST-122).
    /// Optional and defaulted so plan files written before this field
    /// existed continue to load.
    #[serde(default)]
    pub(crate) memory_sync: Option<MemorySyncRecord>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum WorkspacePlanError {
    #[error("invalid plan transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: WorkspacePlanStatus,
        to: WorkspacePlanStatus,
    },
    #[error("invalid plan input: {0}")]
    InvalidInput(String),
    #[error("implementation blocked: workspace plan is in {status:?} status — {reason}")]
    ImplementationBlocked {
        status: WorkspacePlanStatus,
        reason: String,
    },
    #[error("plan approval does not match: {0}")]
    ApprovalMismatch(String),
    #[error("plan integrity check failed: content hash changed after approval")]
    ContentHashMismatch,
    #[error("workspace plan file is invalid or from an incompatible version")]
    InvalidPersistedPlan,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl PartialEq for WorkspacePlanError {
    fn eq(&self, other: &Self) -> bool {
        self.to_string() == other.to_string()
    }
}

impl Eq for WorkspacePlanError {}
