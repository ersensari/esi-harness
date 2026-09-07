mod common;
use esi_workspace_plan::{
    storage::PersistenceError, MemorySyncOutcome, WorkspacePlan, WorkspacePlanError,
};
use std::fs;
use std::path::Path;

#[test]
fn writer_process() {
    let Ok(root) = std::env::var("ESI_CAS_TEST_ROOT") else {
        return;
    };
    let root = Path::new(&root);
    let slot = std::env::var("ESI_CAS_TEST_SLOT").unwrap();
    let mut plan = if std::env::var("ESI_CAS_TEST_MODE").unwrap() == "create" {
        WorkspacePlan::new(root, "new").unwrap()
    } else {
        WorkspacePlan::load(root).unwrap().unwrap()
    };
    plan.set_title(format!("writer-{slot}")).unwrap();
    fs::write(root.join(format!("ready-{slot}")), "").unwrap();
    common::wait_for(&root.join("go"));
    match plan.save(root) {
        Ok(()) => std::process::exit(0),
        Err(WorkspacePlanError::Persistence(PersistenceError::Conflict)) => std::process::exit(2),
        other => panic!("unexpected commit result: {other:?}"),
    }
}

#[test]
fn independent_process_updates_reject_lost_update() {
    let root = tempfile::tempdir().unwrap();
    WorkspacePlan::new(root.path(), "initial")
        .unwrap()
        .save(root.path())
        .unwrap();
    common::race(root.path(), "update");
    let plan = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(plan.storage_revision(), 2);
    assert!(plan.title().starts_with("writer-"));
}

#[test]
fn independent_process_creates_never_overwrite_each_other() {
    let root = tempfile::tempdir().unwrap();
    common::race(root.path(), "create");
    assert_eq!(
        WorkspacePlan::load(root.path())
            .unwrap()
            .unwrap()
            .storage_revision(),
        1
    );
}

#[test]
fn outbox_only_updates_conflict_even_when_content_and_revision_count_match() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "initial").unwrap();
    plan.save(root.path()).unwrap();
    let mut stale = WorkspacePlan::load(root.path()).unwrap().unwrap();
    let hash = plan.content_hash();
    plan.record_memory_sync(MemorySyncOutcome::Synced);
    plan.save(root.path()).unwrap();
    assert_eq!(plan.content_hash(), hash);
    assert_eq!(plan.revision_count(), stale.revision_count());
    stale.record_memory_sync(MemorySyncOutcome::Pending {
        reason: "old request".into(),
    });
    assert!(matches!(
        stale.save(root.path()),
        Err(WorkspacePlanError::Persistence(PersistenceError::Conflict))
    ));
    assert_eq!(stale.storage_revision(), 1);
    assert!(WorkspacePlan::load(root.path())
        .unwrap()
        .unwrap()
        .memory_already_synced());
}

#[test]
fn legacy_plan_load_is_read_only_and_first_save_migrates() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "legacy").unwrap();
    plan.set_requirements(vec![esi_workspace_plan::Requirement {
        id: "REQ-1".into(),
        description: "Preserve approval".into(),
        acceptance_criteria: vec!["Migration does not invalidate approved content".into()],
        priority: esi_workspace_plan::Priority::Must,
    }])
    .unwrap();
    plan.set_plan_content("Approved implementation", "Local persistence", vec![])
        .unwrap();
    plan.approve("operator").unwrap();
    let mut legacy = serde_json::to_value(&plan).unwrap();
    legacy["schema_version"] = 1.into();
    legacy.as_object_mut().unwrap().remove("storage_revision");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let path = WorkspacePlan::plan_path(root.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &bytes).unwrap();
    let mut a = WorkspacePlan::load(root.path()).unwrap().unwrap();
    let mut b = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(a.storage_revision(), 0);
    assert!(a.require_approved_plan().is_ok());
    let hash = a.content_hash();
    a.save(root.path()).unwrap();
    assert_eq!(a.content_hash(), hash);
    assert!(WorkspacePlan::load(root.path())
        .unwrap()
        .unwrap()
        .require_approved_plan()
        .is_ok());
    assert!(matches!(
        b.save(root.path()),
        Err(WorkspacePlanError::Persistence(PersistenceError::Conflict))
    ));
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(stored["schema_version"], 2);
    assert_eq!(stored["storage_revision"], 1);
}

#[test]
fn revision_overflow_leaves_disk_and_memory_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let mut value =
        serde_json::to_value(WorkspacePlan::new(root.path(), "limit").unwrap()).unwrap();
    value["storage_revision"] = u64::MAX.into();
    let path = WorkspacePlan::plan_path(root.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    fs::write(&path, &bytes).unwrap();
    let mut plan = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert!(matches!(
        plan.save(root.path()),
        Err(WorkspacePlanError::Persistence(
            PersistenceError::RevisionExhausted
        ))
    ));
    assert_eq!(plan.storage_revision(), u64::MAX);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn missing_main_never_promotes_pending_and_corruption_is_not_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let path = WorkspacePlan::plan_path(root.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path.with_file_name("workspace-plan.json.pending"),
        b"partial",
    )
    .unwrap();
    assert!(WorkspacePlan::load(root.path()).unwrap().is_none());
    let mut plan = WorkspacePlan::new(root.path(), "safe").unwrap();
    plan.save(root.path()).unwrap();
    fs::write(&path, b"{broken").unwrap();
    assert!(WorkspacePlan::load(root.path()).is_err());
    assert!(matches!(
        plan.save(root.path()),
        Err(WorkspacePlanError::Persistence(PersistenceError::Conflict))
    ));
    assert_eq!(fs::read(path).unwrap(), b"{broken");
}

#[test]
fn loaded_and_deserialized_snapshots_cannot_overwrite_other_targets() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "safe").unwrap();
    plan.save(root.path()).unwrap();
    assert!(matches!(
        plan.save(other.path()),
        Err(WorkspacePlanError::Persistence(PersistenceError::Conflict))
    ));
    let mut imported: WorkspacePlan =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    assert!(matches!(
        imported.save(root.path()),
        Err(WorkspacePlanError::Persistence(PersistenceError::Conflict))
    ));
}
