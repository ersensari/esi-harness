use esi_development::{
    DevelopmentEventKind, DevelopmentStage, DevelopmentState, FailureCategory, HumanGateReason,
    ValidationOutcome,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, InitializeResult,
        ListResourcesResult, MetaObject, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
        ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEVELOPMENT_LOOP_RESOURCE_URI: &str = "ui://esi-development/run";
pub const MCP_APPS_MIME_TYPE: &str = "text/html;profile=mcp-app";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VisualizerStatus {
    Empty,
    Running,
    Failed,
    Blocked,
    Completed,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StageHistoryItem {
    pub sequence: u64,
    pub stage: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WorktreeView {
    pub repository_id: String,
    pub session_id: String,
    pub branch: String,
    pub worktree_path: String,
    pub head: String,
    pub snapshot_id: String,
    pub dirty: bool,
    pub changed_files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ValidationEvidenceView {
    pub attempt: u32,
    pub validator_id: String,
    pub category: String,
    pub command: Vec<String>,
    pub required: bool,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub failure_fingerprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FingerprintView {
    pub fingerprint: String,
    pub category: Option<String>,
    pub source_id: Option<String>,
    pub summary: Option<String>,
    pub occurrences: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct RepairBudgetView {
    pub category: String,
    pub attempts: u32,
    pub base_budget: u32,
    pub extensions: u32,
    pub remaining: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ApprovalView {
    pub gate_id: String,
    pub state: String,
    pub summary: String,
    pub approved_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct EventView {
    pub sequence: u64,
    pub stage: String,
    pub kind: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DevelopmentLoopView {
    pub status: VisualizerStatus,
    pub run_id: Option<String>,
    pub objective: Option<String>,
    pub current_stage: Option<String>,
    pub stage_history: Vec<StageHistoryItem>,
    pub worktree: Option<WorktreeView>,
    pub validation_evidence: Vec<ValidationEvidenceView>,
    pub fingerprints: Vec<FingerprintView>,
    pub repair_budgets: Vec<RepairBudgetView>,
    pub approvals: Vec<ApprovalView>,
    pub events: Vec<EventView>,
}

#[derive(Clone, Default)]
struct FingerprintDetails {
    category: Option<String>,
    source_id: Option<String>,
    summary: Option<String>,
}

impl DevelopmentLoopView {
    pub fn empty() -> Self {
        Self {
            status: VisualizerStatus::Empty,
            run_id: None,
            objective: None,
            current_stage: None,
            stage_history: Vec::new(),
            worktree: None,
            validation_evidence: Vec::new(),
            fingerprints: Vec::new(),
            repair_budgets: Vec::new(),
            approvals: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, esi_development::DevelopmentError> {
        DevelopmentState::load(path.into()).map(|state| Self::from_state(&state))
    }

    pub fn from_state(state: &DevelopmentState) -> Self {
        let status = match state.stage() {
            DevelopmentStage::Completed => VisualizerStatus::Completed,
            DevelopmentStage::Abandoned => VisualizerStatus::Abandoned,
            DevelopmentStage::HumanGate => VisualizerStatus::Blocked,
            DevelopmentStage::Diagnose | DevelopmentStage::Repair
                if state.pending_failure().is_some() =>
            {
                VisualizerStatus::Failed
            }
            _ => VisualizerStatus::Running,
        };
        let stage_history = state
            .events()
            .iter()
            .filter_map(|event| match event.event {
                DevelopmentEventKind::RunStarted => Some(StageHistoryItem {
                    sequence: event.sequence,
                    stage: stage_name(event.stage).to_string(),
                }),
                DevelopmentEventKind::StageTransition { to, .. } => Some(StageHistoryItem {
                    sequence: event.sequence,
                    stage: stage_name(to).to_string(),
                }),
                _ => None,
            })
            .collect();
        let worktree = state.worktree().and_then(|binding| {
            state.worktree_snapshot().map(|snapshot| WorktreeView {
                repository_id: binding.identity.repository_id.clone(),
                session_id: binding.identity.session_id.as_str().to_string(),
                branch: binding.identity.branch.clone(),
                worktree_path: binding
                    .identity
                    .worktree_path
                    .to_string_lossy()
                    .into_owned(),
                head: snapshot.head.clone(),
                snapshot_id: snapshot.snapshot_id.clone(),
                dirty: snapshot.dirty,
                changed_files: snapshot.changed_files.clone(),
            })
        });
        let validation_evidence = state
            .validation_runs()
            .iter()
            .flat_map(|run| {
                run.evidence
                    .iter()
                    .map(move |evidence| ValidationEvidenceView {
                        attempt: run.attempt,
                        validator_id: evidence.validator_id.clone(),
                        category: validation_category_name(evidence.category).to_string(),
                        command: evidence.command.clone(),
                        required: evidence.required,
                        outcome: validation_outcome_name(evidence.outcome).to_string(),
                        exit_code: evidence.exit_code,
                        output: if evidence.stderr.is_empty() {
                            evidence.stdout.clone()
                        } else {
                            evidence.stderr.clone()
                        },
                        failure_fingerprint: evidence
                            .failure_fingerprint
                            .as_ref()
                            .map(|fingerprint| fingerprint.as_str().to_string()),
                    })
            })
            .collect();
        let fingerprints = fingerprint_views(state);
        let repair_budgets = state
            .repair_policy()
            .budgets
            .iter()
            .map(|(category, base_budget)| {
                let attempts = state
                    .repair_attempts()
                    .get(category)
                    .copied()
                    .unwrap_or_default();
                let extensions = state
                    .repair_extensions()
                    .get(category)
                    .copied()
                    .unwrap_or_default();
                RepairBudgetView {
                    category: failure_category_name(*category).to_string(),
                    attempts,
                    base_budget: *base_budget,
                    extensions,
                    remaining: (*base_budget + extensions).saturating_sub(attempts),
                }
            })
            .collect();
        let approvals = approval_views(state);
        let events = state
            .events()
            .iter()
            .map(|event| {
                let (kind, detail) = event_description(&event.event);
                EventView {
                    sequence: event.sequence,
                    stage: stage_name(event.stage).to_string(),
                    kind: kind.to_string(),
                    detail,
                }
            })
            .collect();

        Self {
            status,
            run_id: Some(state.run_id().to_string()),
            objective: state.brief().map(|brief| brief.objective.clone()),
            current_stage: Some(stage_name(state.stage()).to_string()),
            stage_history,
            worktree,
            validation_evidence,
            fingerprints,
            repair_budgets,
            approvals,
            events,
        }
    }
}

fn fingerprint_views(state: &DevelopmentState) -> Vec<FingerprintView> {
    let mut details: BTreeMap<&str, FingerprintDetails> = BTreeMap::new();
    for run in state.validation_runs() {
        for evidence in &run.evidence {
            if let Some(fingerprint) = &evidence.failure_fingerprint {
                details
                    .entry(fingerprint.as_str())
                    .or_insert_with(|| FingerprintDetails {
                        category: Some(validation_category_name(evidence.category).to_string()),
                        source_id: Some(evidence.validator_id.clone()),
                        summary: Some(if evidence.stderr.is_empty() {
                            evidence.stdout.clone()
                        } else {
                            evidence.stderr.clone()
                        }),
                    });
            }
        }
    }
    if let Some(failure) = state.pending_failure() {
        details.insert(
            failure.fingerprint.as_str(),
            FingerprintDetails {
                category: Some(failure_category_name(failure.category).to_string()),
                source_id: Some(failure.source_id.clone()),
                summary: Some(failure.summary.clone()),
            },
        );
    }
    state
        .fingerprint_occurrences()
        .iter()
        .map(|(fingerprint, occurrences)| {
            let detail = details
                .get(fingerprint.as_str())
                .cloned()
                .unwrap_or_default();
            FingerprintView {
                fingerprint: fingerprint.as_str().to_string(),
                category: detail.category,
                source_id: detail.source_id,
                summary: detail.summary,
                occurrences: *occurrences,
            }
        })
        .collect()
}

fn approval_views(state: &DevelopmentState) -> Vec<ApprovalView> {
    let mut approvals: Vec<_> = state
        .events()
        .iter()
        .filter_map(|event| match &event.event {
            DevelopmentEventKind::HumanApprovalRecorded { gate, approved_by } => {
                Some(ApprovalView {
                    gate_id: gate.clone(),
                    state: "completed".to_string(),
                    summary: format!("Approved during {}", stage_name(event.stage)),
                    approved_by: Some(approved_by.clone()),
                })
            }
            _ => None,
        })
        .collect();
    if let Some(gate) = state.pending_human_gate() {
        approvals.push(ApprovalView {
            gate_id: gate.gate_id.clone(),
            state: "pending".to_string(),
            summary: human_gate_summary(&gate.reason),
            approved_by: None,
        });
    } else if state.stage() == DevelopmentStage::CompletionGate {
        approvals.push(ApprovalView {
            gate_id: "completion".to_string(),
            state: "pending".to_string(),
            summary: "Approve the exact validated snapshot for completion".to_string(),
            approved_by: None,
        });
    } else if state.stage() == DevelopmentStage::Plan && state.plan().is_some() {
        approvals.push(ApprovalView {
            gate_id: "worktree_ready".to_string(),
            state: "pending".to_string(),
            summary: "Approve the exact ESI-managed worktree binding".to_string(),
            approved_by: None,
        });
    }
    approvals
}

fn human_gate_summary(reason: &HumanGateReason) -> String {
    match reason {
        HumanGateReason::RepeatedFailure {
            failure,
            occurrences,
        } => format!(
            "Fingerprint {} repeated {} times",
            failure.fingerprint.as_str(),
            occurrences
        ),
        HumanGateReason::RepairBudgetExhausted {
            failure,
            attempts,
            budget,
        } => format!(
            "{} repair budget exhausted after {} of {} attempts",
            failure_category_name(failure.category),
            attempts,
            budget
        ),
        HumanGateReason::AbandonRequested { reason } => {
            format!("Abandonment requested: {reason}")
        }
    }
}

fn event_description(event: &DevelopmentEventKind) -> (&'static str, String) {
    match event {
        DevelopmentEventKind::RunStarted => ("run_started", "Run created".to_string()),
        DevelopmentEventKind::BriefRecorded => ("brief_recorded", "Brief recorded".to_string()),
        DevelopmentEventKind::PlanRecorded => ("plan_recorded", "Plan recorded".to_string()),
        DevelopmentEventKind::StageTransition { from, to } => (
            "stage_transition",
            format!("{} to {}", stage_name(*from), stage_name(*to)),
        ),
        DevelopmentEventKind::WorktreeBound {
            repository_id,
            worktree_path,
            snapshot_id,
        } => (
            "worktree_bound",
            format!(
                "{} at {} ({})",
                repository_id,
                worktree_path.display(),
                snapshot_id
            ),
        ),
        DevelopmentEventKind::WorktreeInspected { snapshot } => (
            "worktree_inspected",
            format!(
                "{} changed files at {}",
                snapshot.changed_files.len(),
                snapshot.snapshot_id
            ),
        ),
        DevelopmentEventKind::ValidationFinished { run } => (
            "validation_finished",
            format!(
                "Attempt {} {}",
                run.attempt,
                if run.passed { "passed" } else { "failed" }
            ),
        ),
        DevelopmentEventKind::FailureRouted {
            failure,
            destination,
        } => (
            "failure_routed",
            format!(
                "{} to {}",
                failure.fingerprint.as_str(),
                stage_name(*destination)
            ),
        ),
        DevelopmentEventKind::ReviewRecorded { approved, summary } => (
            "review_recorded",
            format!(
                "{}: {}",
                if *approved { "approved" } else { "rejected" },
                summary
            ),
        ),
        DevelopmentEventKind::HumanApprovalRecorded { gate, approved_by } => (
            "human_approval_recorded",
            format!("{gate} approved by {approved_by}"),
        ),
    }
}

fn stage_name(stage: DevelopmentStage) -> &'static str {
    match stage {
        DevelopmentStage::Brief => "brief",
        DevelopmentStage::Plan => "plan",
        DevelopmentStage::WorktreeReady => "worktree_ready",
        DevelopmentStage::Implement => "implement",
        DevelopmentStage::DeterministicValidate => "deterministic_validate",
        DevelopmentStage::Diagnose => "diagnose",
        DevelopmentStage::Repair => "repair",
        DevelopmentStage::HumanGate => "human_gate",
        DevelopmentStage::Review => "review",
        DevelopmentStage::CompletionGate => "completion_gate",
        DevelopmentStage::Completed => "completed",
        DevelopmentStage::Abandoned => "abandoned",
    }
}

fn validation_category_name(category: esi_development::ValidationCategory) -> &'static str {
    match category {
        esi_development::ValidationCategory::Scope => "scope",
        esi_development::ValidationCategory::Syntax => "syntax",
        esi_development::ValidationCategory::StaticPolicy => "static_policy",
        esi_development::ValidationCategory::LintTypeBuild => "lint_type_build",
        esi_development::ValidationCategory::TargetedTests => "targeted_tests",
        esi_development::ValidationCategory::BroadTests => "broad_tests",
    }
}

fn failure_category_name(category: FailureCategory) -> &'static str {
    match category {
        FailureCategory::Scope => "scope",
        FailureCategory::Syntax => "syntax",
        FailureCategory::StaticPolicy => "static_policy",
        FailureCategory::Build => "build",
        FailureCategory::Test => "test",
        FailureCategory::Environment => "environment",
        FailureCategory::Review => "review",
    }
}

fn validation_outcome_name(outcome: ValidationOutcome) -> &'static str {
    match outcome {
        ValidationOutcome::Passed => "passed",
        ValidationOutcome::Failed => "failed",
    }
}

pub fn app_html() -> &'static str {
    include_str!("app.html")
}

fn ui_resource_meta() -> MetaObject {
    let mut meta = MetaObject::new();
    meta.0.insert(
        "ui".to_string(),
        json!({ "resourceUri": DEVELOPMENT_LOOP_RESOURCE_URI }),
    );
    meta
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowDevelopmentLoopParams {
    pub state_path: PathBuf,
}

#[derive(Clone)]
pub struct DevelopmentVisualizerServer {
    tool_router: ToolRouter<Self>,
}

impl Default for DevelopmentVisualizerServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for DevelopmentVisualizerServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            "esi-development-visualizer",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "Display persisted ESI development controller state. This server is read-only and has no workflow transition tools.",
        )
    }

    async fn list_resources(
        &self,
        _pagination: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: vec![
                Resource::new(DEVELOPMENT_LOOP_RESOURCE_URI, "ESI Development Run")
                    .with_title("ESI Development Run")
                    .with_description("Read-only local development stages and evidence")
                    .with_mime_type(MCP_APPS_MIME_TYPE),
            ],
            next_cursor: None,
            meta: None,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        params: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        if params.uri != DEVELOPMENT_LOOP_RESOURCE_URI {
            return Err(ErrorData::new(
                ErrorCode::INVALID_REQUEST,
                format!("Unknown resource URI: {}", params.uri),
                None,
            ));
        }
        let mut meta = MetaObject::new();
        meta.0.insert(
            "ui".to_string(),
            json!({
                "prefersBorder": false,
                "csp": {
                    "connectDomains": [],
                    "resourceDomains": [],
                    "frameDomains": [],
                    "baseUriDomains": []
                }
            }),
        );
        Ok(
            ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
                uri: params.uri,
                mime_type: Some(MCP_APPS_MIME_TYPE.to_string()),
                text: app_html().to_string(),
                meta: Some(meta),
            }])
            .into(),
        )
    }
}

#[tool_router(router = tool_router)]
impl DevelopmentVisualizerServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "show_development_loop",
        description = "Render a read-only ESI development run from its persisted controller state file",
        meta = ui_resource_meta()
    )]
    pub async fn show_development_loop(
        &self,
        Parameters(params): Parameters<ShowDevelopmentLoopParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let view = DevelopmentLoopView::load(&params.state_path).map_err(|error| {
            ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                format!("Cannot load ESI development state: {error}"),
                None,
            )
        })?;
        let structured = serde_json::to_value(&view).map_err(|error| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Cannot render ESI development state: {error}"),
                None,
            )
        })?;
        let mut result = CallToolResult::structured(structured);
        result.content = vec![ContentBlock::text(format!(
            "ESI development run {} is {} at {}",
            view.run_id.as_deref().unwrap_or("unknown"),
            serde_json::to_value(view.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string()),
            view.current_stage.as_deref().unwrap_or("unknown")
        ))];
        Ok(result.with_meta(Some(ui_resource_meta())))
    }
}
