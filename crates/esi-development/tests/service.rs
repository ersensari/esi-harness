use esi_development::*;
use esi_workspace_plan::{PlannedTask, PlannedTaskStatus, Priority, Requirement, WorkspacePlan};
use std::{fs, path::Path, process::Command};

struct Fixture {
    root: tempfile::TempDir,
    source: std::path::PathBuf,
    service: ControllerService,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir(&source).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            git(&source, &args);
        }
        fs::write(source.join("result.txt"), "broken\n").unwrap();
        git(&source, &["add", "result.txt"]);
        git(&source, &["commit", "-m", "fixture"]);
        let mut plan = WorkspacePlan::new(&source, "Service fixture").unwrap();
        plan.set_requirements(vec![Requirement {
            id: "R1".into(),
            description: "Fix result".into(),
            acceptance_criteria: vec!["result equals fixed".into()],
            priority: Priority::Must,
        }])
        .unwrap();
        plan.set_plan_content(
            "Repair a result",
            "Local fixture",
            vec![PlannedTask {
                id: "T1".into(),
                title: "Repair".into(),
                description: "Fix result".into(),
                status: PlannedTaskStatus::Pending,
            }],
        )
        .unwrap();
        plan.approve("fixture-human").unwrap();
        plan.save(&source).unwrap();
        let service =
            ControllerService::new(&source, root.path().join("host"), "session".into()).unwrap();
        Self {
            root,
            source,
            service,
        }
    }
    fn start(&self, script: &str) -> DevelopmentState {
        let StartPreparation::Ready(prepared) = self
            .service
            .prepare_start("T1", "start", policy(script))
            .unwrap()
        else {
            panic!("expected start");
        };
        self.service
            .complete_start(*prepared, Some("fixture-human"))
            .unwrap()
    }
}
fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn policy(script: &str) -> ValidationPlan {
    ValidationPlan::new(vec![ValidationCommand {
        id: "test".into(),
        category: ValidationCategory::TargetedTests,
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
        required: true,
    }])
    .unwrap()
}

#[test]
fn real_service_start_replay_failure_repair_and_main_preservation() {
    let fixture = Fixture::new();
    let script = "test \"$(cat result.txt)\" = fixed";
    let state = fixture.start(script);
    let worktree = state.worktree().unwrap().identity.worktree_path.clone();
    assert_eq!(state.stage(), DevelopmentStage::Implement);
    let mut altered_limits = ValidationControl::default();
    altered_limits.timeout = std::time::Duration::from_secs(1);
    assert!(fixture
        .service
        .validate("T1", "altered-policy", &altered_limits)
        .is_err());
    assert!(!fixture
        .service
        .status("T1")
        .unwrap()
        .operations()
        .contains_key("altered-policy"));
    assert!(matches!(
        fixture
            .service
            .prepare_start("T1", "start", policy(script))
            .unwrap(),
        StartPreparation::Recorded(_)
    ));
    assert!(fixture
        .service
        .prepare_start("T1", "other-start", policy(script))
        .is_err());
    assert!(fixture
        .service
        .prepare_start("T1", "start", policy("true"))
        .is_err());
    let failed = fixture
        .service
        .validate("T1", "v1", &ValidationControl::default())
        .unwrap();
    assert_eq!(failed.stage(), DevelopmentStage::Diagnose);
    assert!(!failed.validation_runs()[0].passed);
    let resumed = fixture.service.resume("T1", "resume1").unwrap();
    assert_eq!(resumed.stage(), DevelopmentStage::Repair);
    fs::write(worktree.join("result.txt"), "fixed\n").unwrap();
    let passed = fixture
        .service
        .validate("T1", "v2", &ValidationControl::default())
        .unwrap();
    assert_eq!(passed.stage(), DevelopmentStage::Review);
    assert_eq!(passed.validation_runs().len(), 2);
    let replay = fixture
        .service
        .validate("T1", "v2", &ValidationControl::default())
        .unwrap();
    assert_eq!(replay, passed);
    assert_eq!(
        fs::read_to_string(fixture.source.join("result.txt")).unwrap(),
        "broken\n"
    );
}

#[test]
fn successful_validator_that_changes_files_is_failed_before_review() {
    let fixture = Fixture::new();
    fixture.start("printf changed > result.txt");
    let state = fixture
        .service
        .validate("T1", "v1", &ValidationControl::default())
        .unwrap();
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    let run = state.validation_runs().last().unwrap();
    assert!(!run.passed);
    assert_eq!(
        run.evidence.last().unwrap().validator_id,
        "esi-post-validation-snapshot"
    );
    assert!(run.evidence[0].exit_code == Some(0));
}

#[test]
fn dropped_start_reconciles_without_new_worktree_or_faked_approval() {
    let fixture = Fixture::new();
    let StartPreparation::Ready(prepared) = fixture
        .service
        .prepare_start("T1", "start", policy("true"))
        .unwrap()
    else {
        panic!();
    };
    let path = prepared.inspection().record.identity.worktree_path.clone();
    assert!(fixture.service.resume("T1", "resume-live").is_err());
    drop(prepared);
    let state = fixture.service.resume("T1", "resume-crash").unwrap();
    assert_eq!(
        state.operations()["start"].status,
        OperationStatus::Interrupted
    );
    assert_eq!(state.stage(), DevelopmentStage::Plan);
    assert!(state.worktree().is_none());
    let StartPreparation::Ready(prepared) = fixture
        .service
        .prepare_start("T1", "start2", policy("true"))
        .unwrap()
    else {
        panic!();
    };
    assert_eq!(prepared.inspection().record.identity.worktree_path, path);
    let denied = fixture.service.complete_start(*prepared, None).unwrap();
    assert_eq!(denied.stage(), DevelopmentStage::Plan);
}

#[test]
fn stale_plan_and_wrong_session_cannot_validate_or_resume() {
    let fixture = Fixture::new();
    fixture.start("true");
    let other = ControllerService::new(
        &fixture.source,
        fixture.root.path().join("host"),
        "other-session".into(),
    )
    .unwrap();
    assert!(other.status("T1").is_err());
    assert!(other
        .validate("T1", "wrong", &ValidationControl::default())
        .is_err());
    let mut plan = WorkspacePlan::load(&fixture.source).unwrap().unwrap();
    plan.request_revision("Changed scope").unwrap();
    plan.save(&fixture.source).unwrap();
    assert!(fixture
        .service
        .validate("T1", "stale", &ValidationControl::default())
        .is_err());
    assert!(fixture.service.resume("T1", "stale",).is_err());
    assert_eq!(
        fixture
            .service
            .status("T1")
            .unwrap()
            .validation_runs()
            .len(),
        0
    );
}

#[test]
fn cancelled_validation_is_not_pass_and_does_not_respawn_on_replay() {
    let fixture = Fixture::new();
    let state = fixture.start("touch must-not-exist");
    let control = ValidationControl::default();
    control.cancel();
    let failed = fixture
        .service
        .validate("T1", "cancelled", &control)
        .unwrap();
    assert!(!failed.validation_runs()[0].passed);
    assert!(!state
        .worktree()
        .unwrap()
        .identity
        .worktree_path
        .join("must-not-exist")
        .exists());
    assert_eq!(
        fixture
            .service
            .validate("T1", "cancelled", &ValidationControl::default())
            .unwrap(),
        failed
    );
}

#[test]
fn validator_created_fifo_fails_post_inspection_without_blocking() {
    let fixture = Fixture::new();
    fixture.start("mkfifo unreadable");
    let began = std::time::Instant::now();
    let state = fixture
        .service
        .validate("T1", "fifo", &ValidationControl::default())
        .unwrap();
    assert!(began.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(state.stage(), DevelopmentStage::Diagnose);
    assert!(!state.validation_runs()[0].passed);
}

#[test]
fn contained_validator_cannot_read_host_sentinel_and_uses_fixed_environment() {
    let fixture = Fixture::new();
    let sentinel = fixture.root.path().join("host-only");
    fs::write(&sentinel, "private fixture").unwrap();
    let script = format!(
        "test ! -e '{}' && test \"$HOME\" = /tmp/home && test \"$PATH\" = /usr/bin:/bin",
        sentinel.display()
    );
    fixture.start(&script);
    let state = fixture
        .service
        .validate("T1", "isolated", &ValidationControl::default())
        .unwrap();
    assert!(state.validation_runs()[0].passed);
}

#[test]
fn contained_validator_cannot_connect_to_host_loopback() {
    let fixture = Fixture::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let script = format!("python3 -c 'import socket\ntry:\n socket.create_connection((\"127.0.0.1\", {port}), timeout=0.2)\nexcept OSError:\n pass\nelse:\n raise RuntimeError(\"host network exposed\")'");
    fixture.start(&script);
    let state = fixture
        .service
        .validate("T1", "network", &ValidationControl::default())
        .unwrap();
    assert!(state.validation_runs()[0].passed);
}

#[test]
fn managed_lease_rejects_wrong_run_and_excludes_validation_until_released() {
    let fixture = Fixture::new();
    let state = fixture.start("true");
    assert!(fixture
        .service
        .lease_implementation("T1", "wrong-run")
        .is_err());
    let lease = fixture
        .service
        .lease_implementation("T1", state.run_id())
        .unwrap();
    assert_eq!(
        lease.inspection().record.identity.worktree_path,
        state.worktree().unwrap().identity.worktree_path
    );
    assert!(fixture
        .service
        .validate("T1", "concurrent", &ValidationControl::default())
        .is_err());
    drop(lease);
    assert!(
        fixture
            .service
            .validate("T1", "after-write", &ValidationControl::default())
            .unwrap()
            .validation_runs()[0]
            .passed
    );
    assert!(fixture
        .service
        .lease_implementation("T1", state.run_id())
        .is_err());
}

#[test]
fn interrupted_operation_requires_reconciliation_before_managed_writes() {
    let fixture = Fixture::new();
    let mut state = fixture.start("true");
    let mut request = state.operations()["start"].request.clone();
    request.request_id = "crashed-validation".into();
    request.kind = OperationKind::Validate;
    state
        .reserve_operation(fixture.service.state_path("T1").unwrap(), request)
        .unwrap();
    assert!(fixture
        .service
        .lease_implementation("T1", state.run_id())
        .is_err());
    fixture.service.resume("T1", "reconcile").unwrap();
    assert!(fixture
        .service
        .lease_implementation("T1", state.run_id())
        .is_ok());
}
