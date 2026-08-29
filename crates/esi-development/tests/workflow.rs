use esi_development::*;
use esi_workspace::{
    LifecycleState, SessionId, WorktreeIdentity, WorktreeInspection, WorktreeRecord,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn command(
    id: &str,
    category: ValidationCategory,
    program: &str,
    arguments: &[&str],
) -> ValidationCommand {
    ValidationCommand {
        id: id.to_string(),
        category,
        program: program.to_string(),
        arguments: arguments.iter().map(|value| value.to_string()).collect(),
        required: true,
    }
}

fn inspection(worktree: &Path, snapshot_id: &str) -> WorktreeInspection {
    WorktreeInspection {
        record: WorktreeRecord {
            identity: WorktreeIdentity {
                schema_version: 1,
                session_id: SessionId::new("session-1").unwrap(),
                repository_id: "repository-1".to_string(),
                source_repository: PathBuf::from("/source"),
                main_worktree: PathBuf::from("/source"),
                worktree_path: worktree.to_path_buf(),
                branch: "esi/session-1".to_string(),
                base_commit: "base".to_string(),
                main_head_at_creation: "base".to_string(),
                main_was_dirty: true,
            },
            state: LifecycleState::Ready,
        },
        head: "base".to_string(),
        dirty: false,
        changed_files: Vec::new(),
        snapshot_id: snapshot_id.to_string(),
    }
}

fn ready_state(
    worktree: &Path,
    validation: ValidationPlan,
    policy: RepairPolicy,
) -> (DevelopmentState, WorktreeInspection) {
    let inspection = inspection(worktree, "snapshot-1");
    let mut state = DevelopmentState::new("run-1", policy).unwrap();
    state
        .record_brief(Brief {
            objective: "Implement the requested change".to_string(),
            acceptance_criteria: vec!["Required validation passes".to_string()],
        })
        .unwrap();
    state
        .record_plan(ImplementationPlan {
            summary: "Implement and validate".to_string(),
            validation,
        })
        .unwrap();
    state
        .approve_worktree(
            &inspection,
            WorktreeReadyApproval {
                run_id: "run-1".to_string(),
                repository_id: "repository-1".to_string(),
                worktree_path: worktree.to_path_buf(),
                snapshot_id: "snapshot-1".to_string(),
                approved_by: "human@example.com".to_string(),
            },
        )
        .unwrap();
    state.begin_implementation().unwrap();
    (state, inspection)
}

#[test]
fn transition_table_allows_only_declared_edges() {
    use DevelopmentStage::*;
    let stages = [
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
    ];
    let allowed = [
        (Brief, Plan),
        (Brief, HumanGate),
        (Plan, WorktreeReady),
        (Plan, HumanGate),
        (WorktreeReady, Implement),
        (WorktreeReady, HumanGate),
        (Implement, DeterministicValidate),
        (Implement, HumanGate),
        (DeterministicValidate, Diagnose),
        (DeterministicValidate, Review),
        (Diagnose, Repair),
        (Diagnose, HumanGate),
        (Repair, DeterministicValidate),
        (Repair, HumanGate),
        (HumanGate, Repair),
        (HumanGate, Abandoned),
        (Review, Diagnose),
        (Review, CompletionGate),
        (Review, HumanGate),
        (CompletionGate, Completed),
        (CompletionGate, HumanGate),
    ];
    for from in stages {
        for to in stages {
            assert_eq!(
                is_transition_allowed(from, to),
                allowed.contains(&(from, to)),
                "unexpected transition result for {from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn invalid_transition_cannot_skip_worktree_or_validation() {
    let mut state = DevelopmentState::new("run-1", RepairPolicy::default()).unwrap();
    assert!(matches!(
        state.begin_implementation(),
        Err(DevelopmentError::WorktreeNotReady)
    ));
    assert_eq!(state.stage(), DevelopmentStage::Brief);
}

#[test]
fn validators_run_in_order_and_emit_evidence() {
    let temp = TempDir::new().unwrap();
    let order = temp.path().join("order");
    let order_path = order.to_string_lossy();
    let validation = ValidationPlan::new(vec![
        command(
            "scope",
            ValidationCategory::Scope,
            "/bin/sh",
            &["-c", &format!("printf scope >> '{order_path}'")],
        ),
        command(
            "syntax",
            ValidationCategory::Syntax,
            "/bin/sh",
            &["-c", &format!("printf syntax >> '{order_path}'")],
        ),
        command(
            "targeted",
            ValidationCategory::TargetedTests,
            "/bin/sh",
            &["-c", &format!("printf targeted >> '{order_path}'")],
        ),
    ])
    .unwrap();
    let (mut state, inspection) = ready_state(temp.path(), validation, RepairPolicy::default());

    let run = state.validate(&inspection).unwrap();

    assert!(run.passed);
    assert_eq!(fs::read_to_string(order).unwrap(), "scopesyntaxtargeted");
    assert_eq!(state.stage(), DevelopmentStage::Review);
    assert_eq!(run.evidence.len(), 3);
}

#[test]
fn validation_failure_routes_through_diagnose_and_repair() {
    let temp = TempDir::new().unwrap();
    let pass_marker = temp.path().join("pass");
    let validation = ValidationPlan::new(vec![command(
        "syntax",
        ValidationCategory::Syntax,
        "/bin/sh",
        &["-c", &format!("test -f '{}'", pass_marker.display())],
    )])
    .unwrap();
    let (mut state, mut inspection) = ready_state(temp.path(), validation, RepairPolicy::default());

    assert!(!state.validate(&inspection).unwrap().passed);
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::Repair);

    fs::write(pass_marker, "repaired").unwrap();
    inspection.snapshot_id = "snapshot-2".to_string();
    assert!(state.validate(&inspection).unwrap().passed);
    assert_eq!(state.stage(), DevelopmentStage::Review);
}

#[test]
fn repeated_normalized_fingerprint_requires_human_gate() {
    let temp = TempDir::new().unwrap();
    let line_number = temp.path().join("line-number");
    fs::write(&line_number, "12").unwrap();
    let validation = ValidationPlan::new(vec![command(
        "syntax",
        ValidationCategory::Syntax,
        "/bin/sh",
        &[
            "-c",
            &format!(
                "printf 'error at line ' >&2; cat '{}' >&2; exit 1",
                line_number.display()
            ),
        ],
    )])
    .unwrap();
    let (mut state, mut inspection) = ready_state(temp.path(), validation, RepairPolicy::default());

    state.validate(&inspection).unwrap();
    state.diagnose().unwrap();
    fs::write(line_number, "99").unwrap();
    inspection.snapshot_id = "snapshot-2".to_string();
    state.validate(&inspection).unwrap();

    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::HumanGate);
    assert!(matches!(
        &state.pending_human_gate().unwrap().reason,
        HumanGateReason::RepeatedFailure { occurrences: 2, .. }
    ));
}

#[test]
fn category_budget_exhaustion_requires_human_approval() {
    let temp = TempDir::new().unwrap();
    let mut budgets = BTreeMap::new();
    budgets.insert(FailureCategory::Syntax, 1);
    let policy = RepairPolicy {
        budgets,
        repeated_fingerprint_limit: 99,
    };
    let validation = ValidationPlan::new(vec![command(
        "syntax",
        ValidationCategory::Syntax,
        "/bin/false",
        &[],
    )])
    .unwrap();
    let (mut state, mut inspection) = ready_state(temp.path(), validation, policy);

    state.validate(&inspection).unwrap();
    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::Repair);
    inspection.snapshot_id = "snapshot-2".to_string();
    state.validate(&inspection).unwrap();
    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::HumanGate);

    let gate = state.pending_human_gate().cloned().unwrap();
    let failure = state.pending_failure().cloned().unwrap();
    assert!(state
        .approve_additional_repair(RepairApproval {
            run_id: "wrong-run".to_string(),
            gate_id: gate.gate_id.clone(),
            fingerprint: failure.fingerprint.clone(),
            approved_by: "human@example.com".to_string(),
        })
        .is_err());
    state
        .approve_additional_repair(RepairApproval {
            run_id: "run-1".to_string(),
            gate_id: gate.gate_id,
            fingerprint: failure.fingerprint,
            approved_by: "human@example.com".to_string(),
        })
        .unwrap();
    assert_eq!(state.stage(), DevelopmentStage::Repair);
}

#[test]
fn persisted_state_resumes_with_typed_event_history() {
    let temp = TempDir::new().unwrap();
    let state_path = temp.path().join("state.json");
    let validation = ValidationPlan::new(vec![command(
        "scope",
        ValidationCategory::Scope,
        "/bin/true",
        &[],
    )])
    .unwrap();
    let (mut state, inspection) = ready_state(temp.path(), validation, RepairPolicy::default());
    let mut inspection = inspection;
    inspection.dirty = true;
    inspection.changed_files = vec!["src/lib.rs".to_string(), "tests/workflow.rs".to_string()];
    state.validate(&inspection).unwrap();
    state.save(&state_path).unwrap();

    let resumed = DevelopmentState::load(&state_path).unwrap();

    assert_eq!(resumed, state);
    assert_eq!(resumed.stage(), DevelopmentStage::Review);
    assert_eq!(
        resumed.worktree_snapshot().unwrap().changed_files,
        ["src/lib.rs", "tests/workflow.rs"]
    );
    assert!(resumed.events().iter().any(|event| matches!(
        &event.event,
        DevelopmentEventKind::WorktreeInspected { snapshot }
            if snapshot.changed_files == ["src/lib.rs", "tests/workflow.rs"]
    )));
    assert!(resumed
        .events()
        .iter()
        .enumerate()
        .all(|(index, event)| event.sequence == index as u64 + 1));
}

#[test]
fn schema_one_state_migrates_to_a_typed_worktree_snapshot_event() {
    let temp = TempDir::new().unwrap();
    let state_path = temp.path().join("state-v1.json");
    let validation = ValidationPlan::new(vec![command(
        "scope",
        ValidationCategory::Scope,
        "/bin/true",
        &[],
    )])
    .unwrap();
    let (state, _) = ready_state(temp.path(), validation, RepairPolicy::default());
    let mut legacy = serde_json::to_value(state).unwrap();
    legacy["schema_version"] = serde_json::json!(1);
    legacy.as_object_mut().unwrap().remove("worktree_snapshot");
    let events = legacy["events"].as_array_mut().unwrap();
    events.retain(|event| event["event"]["kind"] != "worktree_inspected");
    for (index, event) in events.iter_mut().enumerate() {
        event["sequence"] = serde_json::json!(index + 1);
    }
    fs::write(&state_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let migrated = DevelopmentState::load(&state_path).unwrap();

    assert_eq!(
        migrated.worktree_snapshot().unwrap().changed_files,
        Vec::<String>::new()
    );
    assert!(matches!(
        migrated.events().last().unwrap().event,
        DevelopmentEventKind::WorktreeInspected { .. }
    ));
}

#[test]
fn worktree_binding_rejects_another_repository() {
    let temp = TempDir::new().unwrap();
    let validation = ValidationPlan::new(vec![command(
        "scope",
        ValidationCategory::Scope,
        "/bin/true",
        &[],
    )])
    .unwrap();
    let (mut state, inspection) = ready_state(temp.path(), validation, RepairPolicy::default());
    let mut other = inspection.clone();
    other.record.identity.repository_id = "other-repository".to_string();

    assert!(matches!(
        state.validate(&other),
        Err(DevelopmentError::WorktreeBindingMismatch)
    ));
    assert_eq!(state.stage(), DevelopmentStage::Implement);
}

#[test]
fn reviewer_rejection_routes_back_through_diagnosis() {
    let temp = TempDir::new().unwrap();
    let validation = ValidationPlan::new(vec![command(
        "scope",
        ValidationCategory::Scope,
        "/bin/true",
        &[],
    )])
    .unwrap();
    let (mut state, inspection) = ready_state(temp.path(), validation, RepairPolicy::default());
    state.validate(&inspection).unwrap();

    state
        .record_review(
            &inspection,
            ReviewDecision::Rejected {
                findings: "missing regression test at line 42".to_string(),
            },
        )
        .unwrap();

    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    assert_eq!(
        state.pending_failure().unwrap().category,
        FailureCategory::Review
    );
    assert_eq!(state.diagnose().unwrap(), DevelopmentStage::Repair);
}

#[test]
fn completion_and_abandonment_require_exact_human_approvals() {
    let temp = TempDir::new().unwrap();
    let validation = ValidationPlan::new(vec![command(
        "scope",
        ValidationCategory::Scope,
        "/bin/true",
        &[],
    )])
    .unwrap();
    let (mut state, mut inspection) = ready_state(temp.path(), validation, RepairPolicy::default());
    state.validate(&inspection).unwrap();
    state
        .record_review(
            &inspection,
            ReviewDecision::Approved {
                summary: "review passed".to_string(),
            },
        )
        .unwrap();

    inspection.snapshot_id = "changed-after-validation".to_string();
    assert!(matches!(
        state.approve_completion(
            &inspection,
            CompletionApproval {
                run_id: "run-1".to_string(),
                snapshot_id: inspection.snapshot_id.clone(),
                approved_by: "human@example.com".to_string(),
            }
        ),
        Err(DevelopmentError::ValidatedSnapshotChanged)
    ));
    inspection.snapshot_id = "snapshot-1".to_string();
    state
        .approve_completion(
            &inspection,
            CompletionApproval {
                run_id: "run-1".to_string(),
                snapshot_id: "snapshot-1".to_string(),
                approved_by: "human@example.com".to_string(),
            },
        )
        .unwrap();
    assert_eq!(state.stage(), DevelopmentStage::Completed);

    let mut abandoned = DevelopmentState::new("run-2", RepairPolicy::default()).unwrap();
    let gate_id = abandoned.request_abandon("user cancelled").unwrap();
    assert!(abandoned
        .approve_abandon(AbandonApproval {
            run_id: "run-2".to_string(),
            gate_id: "wrong-gate".to_string(),
            approved_by: "human@example.com".to_string(),
        })
        .is_err());
    abandoned
        .approve_abandon(AbandonApproval {
            run_id: "run-2".to_string(),
            gate_id,
            approved_by: "human@example.com".to_string(),
        })
        .unwrap();
    assert_eq!(abandoned.stage(), DevelopmentStage::Abandoned);
}

#[test]
fn validation_plan_rejects_out_of_order_categories() {
    assert!(matches!(
        ValidationPlan::new(vec![
            command("tests", ValidationCategory::TargetedTests, "/bin/true", &[],),
            command("syntax", ValidationCategory::Syntax, "/bin/true", &[],),
        ]),
        Err(DevelopmentError::InvalidValidationPlan(_))
    ));
}
