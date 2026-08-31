use esi_development::{
    AbandonApproval, Brief, CompletionApproval, DevelopmentStage, DevelopmentState,
    ImplementationPlan, RepairPolicy, ReviewDecision, ValidationCategory, ValidationCommand,
    ValidationPlan, WorktreeReadyApproval,
};
use esi_development_visualizer::{
    app_html, DevelopmentLoopView, DevelopmentVisualizerServer, ShowDevelopmentLoopParams,
    VisualizerStatus, DEVELOPMENT_LOOP_RESOURCE_URI,
};
use esi_workspace::{
    LifecycleState, SessionId, WorktreeIdentity, WorktreeInspection, WorktreeRecord,
};
use esi_workspace_plan::{
    InnovationDiscovery, PlannedTask, PlannedTaskStatus, Priority, Requirement, WorkspacePlan,
};
use rmcp::handler::server::wrapper::Parameters;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn source_path(worktree: &Path) -> PathBuf {
    worktree.join("source")
}

fn inspection(worktree: &Path) -> WorktreeInspection {
    let source = source_path(worktree);
    WorktreeInspection {
        record: WorktreeRecord {
            identity: WorktreeIdentity {
                schema_version: 1,
                session_id: SessionId::new("session-visualizer").unwrap(),
                repository_id: "repository-visualizer".to_string(),
                source_repository: source.clone(),
                main_worktree: source,
                worktree_path: worktree.to_path_buf(),
                branch: "esi/session-visualizer".to_string(),
                base_commit: "base".to_string(),
                main_head_at_creation: "base".to_string(),
                main_was_dirty: false,
            },
            state: LifecycleState::Ready,
        },
        head: "head-1".to_string(),
        dirty: false,
        changed_files: Vec::new(),
        snapshot_id: "snapshot-1".to_string(),
    }
}

fn state_with_validator(worktree: &Path, program: &str, policy: RepairPolicy) -> DevelopmentState {
    let source = source_path(worktree);
    fs::create_dir_all(&source).unwrap();
    let mut workspace_plan = WorkspacePlan::new(&source, "Visualizer fixture").unwrap();
    workspace_plan
        .set_requirements(vec![Requirement {
            id: "REQ-001".to_string(),
            description: "Show the workspace plan".to_string(),
            acceptance_criteria: vec!["Plan status is visible".to_string()],
            priority: Priority::Must,
        }])
        .unwrap();
    workspace_plan
        .set_innovation_discovery(InnovationDiscovery {
            brief: "Compare plan visualization approaches".to_string(),
            research_findings: vec!["Use the controller-bound source workspace".to_string()],
            candidates: vec!["Inline panel".to_string()],
            selected_rationale: "Keeps workflow evidence together".to_string(),
        })
        .unwrap();
    workspace_plan
        .set_plan_content(
            "Render the plan beside controller evidence",
            "Read-only projection",
            vec![PlannedTask {
                id: "TASK-001".to_string(),
                title: "Render plan".to_string(),
                description: "Add a workspace plan panel".to_string(),
                status: PlannedTaskStatus::Active,
            }],
        )
        .unwrap();
    workspace_plan.approve("planner@example.com").unwrap();
    workspace_plan.save(source).unwrap();
    let inspection = inspection(worktree);
    let mut state = DevelopmentState::new("run-visualizer", policy).unwrap();
    state
        .record_brief(Brief {
            objective: "Render controller evidence".to_string(),
            acceptance_criteria: vec!["The projection stays read-only".to_string()],
        })
        .unwrap();
    state
        .record_plan(ImplementationPlan {
            summary: "Validate and render".to_string(),
            validation: ValidationPlan::new(vec![ValidationCommand {
                id: "targeted".to_string(),
                category: ValidationCategory::TargetedTests,
                program: program.to_string(),
                arguments: Vec::new(),
                required: true,
            }])
            .unwrap(),
        })
        .unwrap();
    state
        .approve_worktree(
            &inspection,
            WorktreeReadyApproval {
                run_id: "run-visualizer".to_string(),
                repository_id: "repository-visualizer".to_string(),
                worktree_path: worktree.to_path_buf(),
                snapshot_id: "snapshot-1".to_string(),
                approved_by: "developer@example.com".to_string(),
            },
        )
        .unwrap();
    state.begin_implementation().unwrap();
    state
}

#[test]
fn view_schema_exposes_every_read_only_section() {
    let schema = serde_json::to_string(&schemars::schema_for!(DevelopmentLoopView)).unwrap();
    for field in [
        "status",
        "current_stage",
        "workspace_plan",
        "stage_history",
        "worktree",
        "validation_evidence",
        "fingerprints",
        "repair_budgets",
        "approvals",
        "events",
    ] {
        assert!(schema.contains(field), "schema missing {field}");
    }
}

#[test]
fn projection_shows_workspace_plan_and_innovation_approval() {
    let temp = TempDir::new().unwrap();
    let state = state_with_validator(temp.path(), "/bin/true", RepairPolicy::default());

    let plan = DevelopmentLoopView::from_state(&state).workspace_plan;

    assert!(plan.exists);
    assert_eq!(plan.status, "approved");
    assert!(plan.implementation_allowed);
    assert_eq!(plan.requirements.len(), 1);
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.innovation.unwrap().candidates, ["Inline panel"]);
    assert_eq!(plan.approval.unwrap().approved_by, "planner@example.com");
}

#[test]
fn projection_uses_controller_worktree_events_and_changed_files() {
    let temp = TempDir::new().unwrap();
    let mut state = state_with_validator(temp.path(), "/bin/true", RepairPolicy::default());
    let mut current = inspection(temp.path());
    current.dirty = true;
    current.changed_files = vec!["src/lib.rs".to_string(), "tests/render.rs".to_string()];
    state.validate(&current).unwrap();

    let view = DevelopmentLoopView::from_state(&state);

    assert_eq!(view.status, VisualizerStatus::Running);
    assert_eq!(view.current_stage.as_deref(), Some("review"));
    assert_eq!(
        view.worktree.unwrap().changed_files,
        ["src/lib.rs", "tests/render.rs"]
    );
    assert!(view
        .events
        .iter()
        .any(|event| event.kind == "worktree_inspected"));
    assert_eq!(view.validation_evidence.len(), 1);
    assert_eq!(view.approvals[0].state, "completed");
}

#[test]
fn projection_distinguishes_failed_and_blocked_runs() {
    let temp = TempDir::new().unwrap();
    let mut policy = RepairPolicy::default();
    policy
        .budgets
        .insert(esi_development::FailureCategory::Test, 0);
    let mut state = state_with_validator(temp.path(), "/bin/false", policy);
    state.validate(&inspection(temp.path())).unwrap();
    assert_eq!(
        DevelopmentLoopView::from_state(&state).status,
        VisualizerStatus::Failed
    );

    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::HumanGate);
    let blocked = DevelopmentLoopView::from_state(&state);
    assert_eq!(blocked.status, VisualizerStatus::Blocked);
    assert!(blocked
        .approvals
        .iter()
        .any(|approval| approval.state == "pending"));
    assert_eq!(blocked.fingerprints.len(), 1);
    assert_eq!(
        blocked
            .repair_budgets
            .iter()
            .find(|item| item.category == "test")
            .unwrap()
            .remaining,
        0
    );
}

#[test]
fn projection_distinguishes_empty_completed_and_abandoned_runs() {
    assert_eq!(DevelopmentLoopView::empty().status, VisualizerStatus::Empty);
    let temp = TempDir::new().unwrap();
    let current = inspection(temp.path());
    let mut completed = state_with_validator(temp.path(), "/bin/true", RepairPolicy::default());
    completed.validate(&current).unwrap();
    completed
        .record_review(
            &current,
            ReviewDecision::Approved {
                summary: "ready".to_string(),
            },
        )
        .unwrap();
    completed
        .approve_completion(
            &current,
            CompletionApproval {
                run_id: "run-visualizer".to_string(),
                snapshot_id: "snapshot-1".to_string(),
                approved_by: "developer@example.com".to_string(),
            },
        )
        .unwrap();
    assert_eq!(
        DevelopmentLoopView::from_state(&completed).status,
        VisualizerStatus::Completed
    );

    let mut abandoned = DevelopmentState::new("run-abandoned", RepairPolicy::default()).unwrap();
    let gate_id = abandoned.request_abandon("No longer needed").unwrap();
    abandoned
        .approve_abandon(AbandonApproval {
            run_id: "run-abandoned".to_string(),
            gate_id,
            approved_by: "developer@example.com".to_string(),
        })
        .unwrap();
    assert_eq!(
        DevelopmentLoopView::from_state(&abandoned).status,
        VisualizerStatus::Abandoned
    );
}

#[test]
fn html_is_self_contained_and_renders_required_content_regions() {
    let html = app_html();
    for content in [
        "Stage history",
        "Workspace plan",
        "Worktree",
        "Changed files",
        "Validation evidence",
        "Fingerprints",
        "Repair budgets",
        "Approvals",
        "Controller events",
        "ui/notifications/tool-result",
    ] {
        assert!(html.contains(content), "HTML missing {content}");
    }
    assert!(!html.contains("<script src="));
    assert!(!html.contains("<link rel="));
    assert!(!html.contains("tools/call"));
}

#[tokio::test]
async fn mcp_tool_loads_validated_persisted_state_as_structured_content() {
    let temp = TempDir::new().unwrap();
    let state_path = temp.path().join("state.json");
    let state = state_with_validator(temp.path(), "/bin/true", RepairPolicy::default());
    state.save(&state_path).unwrap();
    let result = DevelopmentVisualizerServer::new()
        .show_development_loop(Parameters(ShowDevelopmentLoopParams { state_path }))
        .await
        .unwrap();

    assert_eq!(
        result
            .meta
            .as_ref()
            .and_then(|meta| meta.0.get("ui"))
            .and_then(|ui| ui.get("resourceUri"))
            .and_then(|uri| uri.as_str()),
        Some(DEVELOPMENT_LOOP_RESOURCE_URI)
    );
    let structured = result.structured_content.unwrap();
    assert_eq!(structured["run_id"], "run-visualizer");
    assert_eq!(structured["status"], "running");
}
