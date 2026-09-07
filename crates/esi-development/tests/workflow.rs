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

#[cfg(target_os = "linux")]
fn lifecycle_fixture(script: &str) -> (TempDir, DevelopmentState, WorktreeInspection) {
    let directory = TempDir::new().unwrap();
    let plan = ValidationPlan::new(vec![command(
        "lifecycle",
        ValidationCategory::TargetedTests,
        "sh",
        &["-c", script],
    )])
    .unwrap();
    let (state, inspection) = ready_state(directory.path(), plan, RepairPolicy::default());
    (directory, state, inspection)
}

#[cfg(target_os = "linux")]
fn assert_descendant_stopped(evidence: &ValidationEvidence) {
    let pid: u32 = evidence.stdout.trim().parse().unwrap();
    for _ in 0..100 {
        match fs::read_to_string(format!("/proc/{pid}/status")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Ok(status)
                if status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains('Z')) =>
            {
                return
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
    panic!("owned descendant {pid} is still running");
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_timeout_kills_descendants_and_blocks_review() {
    let (directory, mut state, inspection) = lifecycle_fixture("sleep 60 & echo $!; wait");
    let mut control = ValidationControl::default();
    control.timeout = std::time::Duration::from_millis(150);
    let start = std::time::Instant::now();
    let run = state.validate_with_control(&inspection, &control).unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    assert!(!run.passed);
    assert_eq!(
        run.evidence[0].termination,
        Some(ValidationTermination::TimedOut)
    );
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    assert_descendant_stopped(&run.evidence[0]);
    let path = directory.path().join("interrupted-state.json");
    state.save(&path).unwrap();
    let resumed = DevelopmentState::load(&path).unwrap();
    assert_eq!(resumed.stage(), DevelopmentStage::Diagnose);
    assert_eq!(resumed.validation_runs()[0], run);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_cancel_from_another_thread_stops_tree() {
    let (_directory, mut state, inspection) = lifecycle_fixture("sleep 60 & echo $!; wait");
    let control = ValidationControl::default();
    let token = control.clone();
    let cancel = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        token.cancel();
    });
    let start = std::time::Instant::now();
    let run = state.validate_with_control(&inspection, &control).unwrap();
    cancel.join().unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    assert!(!run.passed);
    assert_eq!(
        run.evidence[0].termination,
        Some(ValidationTermination::Cancelled)
    );
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    assert_descendant_stopped(&run.evidence[0]);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_success_cleans_background_descendants() {
    let (_directory, mut state, inspection) = lifecycle_fixture("sleep 60 & echo $!; exit 0");
    let run = state.validate(&inspection).unwrap();
    assert!(run.passed);
    assert_descendant_stopped(&run.evidence[0]);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_drains_both_streams_with_bounded_evidence() {
    let (_directory, mut state, inspection) =
        lifecycle_fixture("head -c 200000 /dev/zero & head -c 200000 /dev/zero >&2 & wait");
    let mut control = ValidationControl::default();
    control.output_limit_bytes = 1024;
    let run = state.validate_with_control(&inspection, &control).unwrap();
    let evidence = &run.evidence[0];
    assert!(run.passed);
    assert_eq!(evidence.stdout.len(), 1024);
    assert_eq!(evidence.stderr.len(), 1024);
    assert!(evidence.stdout_truncated && evidence.stderr_truncated);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_infinite_output_times_out_without_unbounded_capture() {
    let (_directory, mut state, inspection) = lifecycle_fixture("cat /dev/zero");
    let mut control = ValidationControl::default();
    control.timeout = std::time::Duration::from_millis(100);
    control.output_limit_bytes = 511;
    let run = state.validate_with_control(&inspection, &control).unwrap();
    assert!(!run.passed);
    assert_eq!(
        run.evidence[0].termination,
        Some(ValidationTermination::TimedOut)
    );
    assert!(run.evidence[0].stdout.len() <= 511);
    assert!(run.evidence[0].stdout_truncated);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_precancel_optional_does_not_spawn_or_pass() {
    let directory = TempDir::new().unwrap();
    let mut optional = command(
        "optional",
        ValidationCategory::Syntax,
        "sh",
        &["-c", "touch must-not-exist"],
    );
    optional.required = false;
    let required = command("required", ValidationCategory::TargetedTests, "true", &[]);
    let plan = ValidationPlan::new(vec![optional, required]).unwrap();
    let (mut state, inspection) = ready_state(directory.path(), plan, RepairPolicy::default());
    let control = ValidationControl::default();
    control.cancel();
    let run = state.validate_with_control(&inspection, &control).unwrap();
    assert!(!run.passed);
    assert_eq!(run.evidence.len(), 1);
    assert!(!directory.path().join("must-not-exist").exists());
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_utf8_and_legacy_evidence_are_compatible() {
    let (_directory, mut state, inspection) = lifecycle_fixture("printf '\\377\\377\\377'");
    let mut control = ValidationControl::default();
    control.output_limit_bytes = 4;
    let run = state.validate_with_control(&inspection, &control).unwrap();
    assert!(run.passed);
    assert!(run.evidence[0].stdout.len() <= 4);
    let mut json = serde_json::to_value(&run.evidence[0]).unwrap();
    for key in ["termination", "stdout_truncated", "stderr_truncated"] {
        json.as_object_mut().unwrap().remove(key);
    }
    let old: ValidationEvidence = serde_json::from_value(json).unwrap();
    assert_eq!(old.termination, None);
    assert!(!old.stdout_truncated);
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_invalid_controls_leave_state_unchanged() {
    let (_directory, mut state, inspection) = lifecycle_fixture("exit 0");
    let mut control = ValidationControl::default();
    control.timeout = std::time::Duration::ZERO;
    assert!(state.validate_with_control(&inspection, &control).is_err());
    assert_eq!(state.stage(), DevelopmentStage::Implement);
    assert!(state.validation_runs().is_empty());
}

#[test]
#[cfg(target_os = "linux")]
fn validation_lifecycle_spawn_failure_is_typed_and_cannot_open_review() {
    let directory = TempDir::new().unwrap();
    let plan = ValidationPlan::new(vec![command(
        "missing",
        ValidationCategory::TargetedTests,
        "/nonexistent/esi-validation-fixture",
        &[],
    )])
    .unwrap();
    let (mut state, inspection) = ready_state(directory.path(), plan, RepairPolicy::default());
    let run = state.validate(&inspection).unwrap();
    assert!(!run.passed);
    assert_eq!(
        run.evidence[0].termination,
        Some(ValidationTermination::ProcessError)
    );
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
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

// ---------------------------------------------------------------------------
// Workspace plan gate integration tests (ADR-0010)
// ---------------------------------------------------------------------------

fn source_with_approved_plan() -> TempDir {
    let source_dir = TempDir::new().unwrap();
    let mut plan =
        esi_workspace_plan::WorkspacePlan::new(source_dir.path(), "Test Project").unwrap();
    plan.set_requirements(vec![esi_workspace_plan::Requirement {
        id: "REQ-001".to_string(),
        description: "Test requirement".to_string(),
        acceptance_criteria: vec!["It works".to_string()],
        priority: esi_workspace_plan::Priority::Must,
    }])
    .unwrap();
    plan.set_plan_content("Test plan", "Test architecture", vec![])
        .unwrap();
    plan.approve("test@example.com").unwrap();
    plan.save(source_dir.path()).unwrap();
    source_dir
}

fn inspection_with_source(source: &Path, worktree: &Path, snapshot_id: &str) -> WorktreeInspection {
    WorktreeInspection {
        record: WorktreeRecord {
            identity: WorktreeIdentity {
                schema_version: 1,
                session_id: SessionId::new("session-plan").unwrap(),
                repository_id: "repository-plan".to_string(),
                source_repository: source.to_path_buf(),
                main_worktree: source.to_path_buf(),
                worktree_path: worktree.to_path_buf(),
                branch: "esi/session-plan".to_string(),
                base_commit: "base".to_string(),
                main_head_at_creation: "base".to_string(),
                main_was_dirty: false,
            },
            state: LifecycleState::Ready,
        },
        head: "base".to_string(),
        dirty: false,
        changed_files: Vec::new(),
        snapshot_id: snapshot_id.to_string(),
    }
}

#[test]
fn workspace_plan_gate_blocks_approve_worktree_without_plan() {
    let source_dir = TempDir::new().unwrap();
    let worktree_dir = TempDir::new().unwrap();
    let inspection = inspection_with_source(source_dir.path(), worktree_dir.path(), "snap-1");

    let mut state = DevelopmentState::new("run-plan-1", RepairPolicy::default()).unwrap();
    state
        .record_brief(Brief {
            objective: "Test implementation".to_string(),
            acceptance_criteria: vec!["Gate enforced".to_string()],
        })
        .unwrap();
    state
        .record_plan(ImplementationPlan {
            summary: "Implement and validate".to_string(),
            validation: ValidationPlan::new(vec![command(
                "check",
                ValidationCategory::Syntax,
                "/bin/true",
                &[],
            )])
            .unwrap(),
        })
        .unwrap();

    let result = state.approve_worktree(
        &inspection,
        WorktreeReadyApproval {
            run_id: "run-plan-1".to_string(),
            repository_id: "repository-plan".to_string(),
            worktree_path: worktree_dir.path().to_path_buf(),
            snapshot_id: "snap-1".to_string(),
            approved_by: "human@example.com".to_string(),
        },
    );

    assert!(
        result.is_err(),
        "approve_worktree should fail without workspace plan"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("workspace plan") || err_msg.contains("no workspace plan"),
        "error should mention workspace plan: {err_msg}"
    );
}

#[test]
fn workspace_plan_gate_allows_approve_worktree_with_approved_plan() {
    let source_dir = source_with_approved_plan();
    let worktree_dir = TempDir::new().unwrap();
    let inspection = inspection_with_source(source_dir.path(), worktree_dir.path(), "snap-2");

    let mut state = DevelopmentState::new("run-plan-2", RepairPolicy::default()).unwrap();
    state
        .record_brief(Brief {
            objective: "Test implementation with plan".to_string(),
            acceptance_criteria: vec!["Gate passes".to_string()],
        })
        .unwrap();
    state
        .record_plan(ImplementationPlan {
            summary: "Implement and validate".to_string(),
            validation: ValidationPlan::new(vec![command(
                "check",
                ValidationCategory::Syntax,
                "/bin/true",
                &[],
            )])
            .unwrap(),
        })
        .unwrap();

    state
        .approve_worktree(
            &inspection,
            WorktreeReadyApproval {
                run_id: "run-plan-2".to_string(),
                repository_id: "repository-plan".to_string(),
                worktree_path: worktree_dir.path().to_path_buf(),
                snapshot_id: "snap-2".to_string(),
                approved_by: "human@example.com".to_string(),
            },
        )
        .expect("approve_worktree should succeed with approved workspace plan");

    assert_eq!(state.stage(), DevelopmentStage::WorktreeReady);
}

#[test]
fn workspace_plan_gate_blocks_unapproved_plan() {
    let source_dir = TempDir::new().unwrap();
    // Create a plan in planning status (not approved)
    let mut plan = esi_workspace_plan::WorkspacePlan::new(source_dir.path(), "Unapproved").unwrap();
    plan.set_requirements(vec![esi_workspace_plan::Requirement {
        id: "REQ-001".to_string(),
        description: "Some requirement".to_string(),
        acceptance_criteria: vec!["Criterion".to_string()],
        priority: esi_workspace_plan::Priority::Must,
    }])
    .unwrap();
    plan.set_plan_content("Plan in progress", "Arch", vec![])
        .unwrap();
    // Do NOT approve
    plan.save(source_dir.path()).unwrap();

    let worktree_dir = TempDir::new().unwrap();
    let inspection = inspection_with_source(source_dir.path(), worktree_dir.path(), "snap-3");

    let mut state = DevelopmentState::new("run-plan-3", RepairPolicy::default()).unwrap();
    state
        .record_brief(Brief {
            objective: "Test blocked implementation".to_string(),
            acceptance_criteria: vec!["Gate blocks".to_string()],
        })
        .unwrap();
    state
        .record_plan(ImplementationPlan {
            summary: "Implement and validate".to_string(),
            validation: ValidationPlan::new(vec![command(
                "check",
                ValidationCategory::Syntax,
                "/bin/true",
                &[],
            )])
            .unwrap(),
        })
        .unwrap();

    let result = state.approve_worktree(
        &inspection,
        WorktreeReadyApproval {
            run_id: "run-plan-3".to_string(),
            repository_id: "repository-plan".to_string(),
            worktree_path: worktree_dir.path().to_path_buf(),
            snapshot_id: "snap-3".to_string(),
            approved_by: "human@example.com".to_string(),
        },
    );

    assert!(result.is_err(), "should block with unapproved plan");
}
