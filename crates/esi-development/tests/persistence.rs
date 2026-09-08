#[path = "../../esi-workspace-plan/tests/common/mod.rs"]
mod common;
use esi_development::{DevelopmentError, DevelopmentState, RepairPolicy};
use esi_workspace_plan::storage::PersistenceError;
use std::fs;
use std::path::Path;

#[test]
fn writer_process() {
    let Ok(root) = std::env::var("ESI_CAS_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let path = root.join("state.json");
    let slot = std::env::var("ESI_CAS_TEST_SLOT").unwrap();
    let mut state = if std::env::var("ESI_CAS_TEST_MODE").unwrap() == "create" {
        DevelopmentState::new("run", RepairPolicy::default()).unwrap()
    } else {
        DevelopmentState::load(&path).unwrap()
    };
    state.request_abandon(format!("writer-{slot}")).unwrap();
    fs::write(root.join(format!("ready-{slot}")), "").unwrap();
    common::wait_for(&root.join("go"));
    match state.save(&path) {
        Ok(()) => std::process::exit(0),
        Err(DevelopmentError::Persistence(PersistenceError::Conflict)) => std::process::exit(2),
        other => panic!("unexpected commit result: {other:?}"),
    }
}

#[test]
fn independent_controller_writers_cannot_lose_updates_or_duplicate_creates() {
    for mode in ["create", "update"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.json");
        if mode == "update" {
            DevelopmentState::new("run", RepairPolicy::default())
                .unwrap()
                .save(&path)
                .unwrap();
        }
        common::race(root.path(), mode);
        let state = DevelopmentState::load(&path).unwrap();
        assert_eq!(
            state.storage_revision(),
            if mode == "create" { 1 } else { 2 }
        );
        assert!(state.pending_human_gate().is_some());
    }
}

#[test]
fn legacy_controller_schemas_migrate_without_rewriting_on_load() {
    for version in [1, 2, 3] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("state.json");
        let state = DevelopmentState::new("legacy", RepairPolicy::default()).unwrap();
        let mut legacy = serde_json::to_value(state).unwrap();
        legacy["schema_version"] = version.into();
        legacy.as_object_mut().unwrap().remove("storage_revision");
        legacy.as_object_mut().unwrap().remove("operations");
        let bytes = serde_json::to_vec(&legacy).unwrap();
        fs::write(&path, &bytes).unwrap();
        let mut a = DevelopmentState::load(&path).unwrap();
        let mut stale = DevelopmentState::load(&path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(a.storage_revision(), 0);
        a.save(&path).unwrap();
        assert!(matches!(
            stale.save(&path),
            Err(DevelopmentError::Persistence(PersistenceError::Conflict))
        ));
        let stored: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored["schema_version"], 4);
        assert_eq!(stored["storage_revision"], 1);
    }
}

#[test]
fn pending_controller_bytes_never_replace_valid_state_or_hide_corruption() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    let mut state = DevelopmentState::new("run", RepairPolicy::default()).unwrap();
    state.save(&path).unwrap();
    fs::write(root.path().join("state.json.pending"), b"partial").unwrap();
    assert_eq!(DevelopmentState::load(&path).unwrap(), state);
    state.save(&path).unwrap();
    assert_eq!(state.storage_revision(), 2);
    fs::write(&path, b"broken").unwrap();
    assert!(DevelopmentState::load(&path).is_err());
    assert!(matches!(
        state.save(&path),
        Err(DevelopmentError::Persistence(PersistenceError::Conflict))
    ));
    assert_eq!(fs::read(path).unwrap(), b"broken");
}
