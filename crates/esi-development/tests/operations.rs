use esi_development::{
    Brief, DevelopmentStage, DevelopmentState, OperationKind, OperationRequest,
    OperationReservation, OperationStatus, RepairPolicy,
};

fn request(id: &str) -> OperationRequest {
    OperationRequest {
        request_id: id.into(),
        kind: OperationKind::Start,
        source_plan_hash: "a".repeat(64),
        source_revision: 1,
        task_id: "task-1".into(),
        session_id: "session-1".into(),
        policy_digest: "b".repeat(64),
        expected_snapshot: "snapshot-1".into(),
    }
}

fn state() -> DevelopmentState {
    let mut state = DevelopmentState::new("run-1", RepairPolicy::default()).unwrap();
    state
        .record_brief(Brief {
            objective: "operation fixture".into(),
            acceptance_criteria: vec!["durable intent".into()],
        })
        .unwrap();
    state
}

#[test]
fn identical_retry_replays_without_another_reservation_or_write() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let mut state = state();
    assert_eq!(
        state.reserve_operation(&path, request("start-1")).unwrap(),
        OperationReservation::Reserved
    );
    let bytes = std::fs::read(&path).unwrap();
    let mut reloaded = DevelopmentState::load(&path).unwrap();
    assert!(
        matches!(reloaded.reserve_operation(&path, request("start-1")).unwrap(), OperationReservation::Recorded(r) if r.status == OperationStatus::Started)
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    reloaded
        .finish_operation(&path, "start-1", OperationStatus::Completed)
        .unwrap();
    assert_eq!(reloaded.stage(), DevelopmentStage::Plan);
    assert!(
        matches!(reloaded.reserve_operation(&path, request("start-1")).unwrap(), OperationReservation::Recorded(r) if r.status == OperationStatus::Completed)
    );
    assert_eq!(reloaded.operations().len(), 1);
}

#[test]
fn changed_payload_and_second_active_operation_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let mut state = state();
    state.reserve_operation(&path, request("start-1")).unwrap();
    let before = state.clone();
    let mut changed = request("start-1");
    changed.source_revision += 1;
    assert!(state.reserve_operation(&path, changed).is_err());
    assert!(state.reserve_operation(&path, request("start-2")).is_err());
    assert_eq!(state, before);
    assert_eq!(DevelopmentState::load(&path).unwrap(), before);
}

#[test]
fn crash_intent_is_not_success_and_interruption_does_not_advance_fsm() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    state()
        .reserve_operation(&path, request("start-1"))
        .unwrap();
    let mut recovered = DevelopmentState::load(&path).unwrap();
    assert_eq!(
        recovered.operations()["start-1"].status,
        OperationStatus::Started
    );
    recovered
        .finish_operation(&path, "start-1", OperationStatus::Interrupted)
        .unwrap();
    assert_eq!(recovered.stage(), DevelopmentStage::Plan);
    assert!(recovered.validation_runs().is_empty());
    assert!(
        matches!(recovered.reserve_operation(&path, request("start-1")).unwrap(), OperationReservation::Recorded(r) if r.status == OperationStatus::Interrupted)
    );
    assert!(recovered
        .finish_operation(&path, "start-1", OperationStatus::Completed)
        .is_err());
}

#[test]
fn stale_writers_cannot_reserve_or_finish_and_keep_their_in_memory_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let mut first = state();
    let mut stale = first.clone();
    first.reserve_operation(&path, request("start-1")).unwrap();
    let before = stale.clone();
    assert!(stale.reserve_operation(&path, request("start-2")).is_err());
    assert_eq!(stale, before);
    let mut stale = DevelopmentState::load(&path).unwrap();
    first
        .finish_operation(&path, "start-1", OperationStatus::Failed)
        .unwrap();
    let before = stale.clone();
    assert!(stale
        .finish_operation(&path, "start-1", OperationStatus::Completed)
        .is_err());
    assert_eq!(stale, before);
    assert_eq!(
        DevelopmentState::load(&path).unwrap().operations()["start-1"].status,
        OperationStatus::Failed
    );
}

#[test]
fn invalid_identity_and_illegal_stage_never_create_state_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let mut state = state();
    for id in ["", "../escape", "spaces here"] {
        assert!(state.reserve_operation(&path, request(id)).is_err());
    }
    let mut validate = request("validate-1");
    validate.kind = OperationKind::Validate;
    assert!(state.reserve_operation(&path, validate).is_err());
    assert!(!path.exists());
}

#[test]
fn corrupted_operation_digest_is_rejected_on_load() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    state()
        .reserve_operation(&path, request("start-1"))
        .unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    value["operations"]["start-1"]["request"]["policy_digest"] = "c".repeat(64).into();
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(DevelopmentState::load(&path).is_err());
}
