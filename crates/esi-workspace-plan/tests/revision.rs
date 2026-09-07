use esi_workspace_plan::*;
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, WorkspacePlan) {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Plan title").unwrap();
    plan.set_requirements(
        ["R1", "R2"]
            .into_iter()
            .map(|id| Requirement {
                id: id.into(),
                description: id.into(),
                acceptance_criteria: vec!["Original criterion".into()],
                priority: Priority::Must,
            })
            .collect(),
    )
    .unwrap();
    plan.set_plan_content(
        "Plan",
        "Architecture",
        ["A", "B", "C"]
            .into_iter()
            .map(|id| PlannedTask {
                id: id.into(),
                title: id.into(),
                description: "Implement".into(),
                status: PlannedTaskStatus::Pending,
            })
            .collect(),
    )
    .unwrap();
    plan.set_task_contracts(
        [("A", "R1"), ("B", "R2"), ("C", "R1")]
            .into_iter()
            .map(|(id, requirement)| {
                (
                    id.into(),
                    TaskContract {
                        depends_on: if id == "C" { vec!["A".into()] } else { vec![] },
                        acceptance_criteria: vec![TaskAcceptanceCriterion {
                            id: "AC1".into(),
                            description: "Criterion".into(),
                            requirement_id: Some(requirement.into()),
                        }],
                        ..Default::default()
                    },
                )
            })
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap();
    plan.approve("first-human").unwrap();
    (root, plan)
}

#[test]
fn revision_task_change_marks_downstream_but_not_independent_scope() {
    let (_root, mut plan) = fixture();
    let mut tasks = plan.tasks().to_vec();
    tasks[0].title = "Changed A".into();
    plan.set_plan_content("Plan", "Architecture", tasks)
        .unwrap();
    let diff = plan.revision_diff();
    assert_eq!(diff.baseline, RevisionBaseline::Available);
    assert_eq!(diff.affected_task_ids, ["A", "C"]);
    assert_eq!(diff.changes.len(), 1);
    assert_eq!(diff.changes[0].path, "/tasks/A/title");
    assert_eq!(diff.changes[0].before, Some(serde_json::json!("A")));
    assert_eq!(diff.changes[0].after, Some(serde_json::json!("Changed A")));
    assert!(!plan.is_implementation_allowed());
}

#[test]
fn revision_exposes_every_semantic_category() {
    for (field, path) in [
        (0, "/title"),
        (1, "/description"),
        (2, "/architecture_notes"),
        (3, "/requirements/R1/acceptance_criteria"),
        (4, "/task_contracts/C/depends_on"),
        (5, "/task_contracts/A/acceptance_criteria"),
        (6, "/innovation_discovery"),
    ] {
        let (_root, mut plan) = fixture();
        match field {
            0 => plan.set_title("Changed").unwrap(),
            1 => plan
                .set_plan_content("Changed", "Architecture", plan.tasks().to_vec())
                .unwrap(),
            2 => plan
                .set_plan_content("Plan", "Changed", plan.tasks().to_vec())
                .unwrap(),
            3 => {
                let mut requirements = plan.requirements().to_vec();
                requirements[0]
                    .acceptance_criteria
                    .push("New criterion".into());
                plan.set_requirements(requirements).unwrap();
            }
            4 => {
                let mut contracts = plan.task_contracts().clone();
                contracts.get_mut("C").unwrap().depends_on = vec!["B".into()];
                plan.set_task_contracts(contracts).unwrap();
            }
            5 => {
                let mut contracts = plan.task_contracts().clone();
                contracts.get_mut("A").unwrap().acceptance_criteria[0].description =
                    "Changed".into();
                plan.set_task_contracts(contracts).unwrap();
            }
            _ => plan
                .set_innovation_discovery(InnovationDiscovery {
                    brief: "Alternative".into(),
                    ..Default::default()
                })
                .unwrap(),
        }
        let diff = plan.revision_diff();
        assert!(
            diff.changes.iter().any(|change| change.path == path),
            "missing {path}"
        );
        assert!(!diff.affected_task_ids.is_empty());
        if field == 3 {
            assert_eq!(diff.affected_task_ids, ["A", "C"]);
        }
    }
}

#[test]
fn revision_status_and_outbox_do_not_change_scope_or_history() {
    let (root, mut plan) = fixture();
    let approved = plan.approval_history().to_vec();
    let mut tasks = plan.tasks().to_vec();
    tasks[0].status = PlannedTaskStatus::Completed;
    plan.set_plan_content("Plan", "Architecture", tasks)
        .unwrap();
    plan.record_memory_sync(MemorySyncOutcome::Synced);
    assert!(plan.revision_diff().changes.is_empty());
    assert!(plan.revision_diff().affected_task_ids.is_empty());
    assert!(plan.is_implementation_allowed());
    assert_eq!(plan.approval_history(), approved);
    plan.save(root.path()).unwrap();
    assert_eq!(
        WorkspacePlan::load(root.path())
            .unwrap()
            .unwrap()
            .approval_history(),
        approved
    );
}

#[test]
fn revision_reapproval_appends_history_and_resets_comparison_baseline() {
    let (root, mut plan) = fixture();
    let first = plan.approval_history()[0].clone();
    plan.set_title("Second version").unwrap();
    plan.approve("second-human").unwrap();
    assert_eq!(plan.approval_history().len(), 2);
    assert_eq!(plan.approval_history()[0], first);
    assert!(plan.revision_diff().changes.is_empty());
    plan.save(root.path()).unwrap();
    assert_eq!(
        WorkspacePlan::load(root.path())
            .unwrap()
            .unwrap()
            .approval_history()
            .len(),
        2
    );
}

#[test]
fn revision_stale_approval_cannot_overwrite_a_newer_snapshot() {
    let (root, mut plan) = fixture();
    plan.save(root.path()).unwrap();
    let mut stale = WorkspacePlan::load(root.path()).unwrap().unwrap();
    plan.set_title("Newer scope").unwrap();
    plan.save(root.path()).unwrap();
    stale.request_revision("Reconfirm old view").unwrap();
    stale.approve("stale-human").unwrap();
    assert!(matches!(
        stale.save(root.path()),
        Err(WorkspacePlanError::Persistence(
            storage::PersistenceError::Conflict
        ))
    ));
    let loaded = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(loaded.title(), "Newer scope");
    assert_eq!(loaded.approval_history().len(), 1);
    assert!(!loaded.is_implementation_allowed());
}

#[test]
fn revision_legacy_approved_snapshot_is_captured_without_rewriting_or_reapproving() {
    let (root, plan) = fixture();
    let mut legacy = serde_json::to_value(&plan).unwrap();
    legacy["schema_version"] = 3.into();
    legacy.as_object_mut().unwrap().remove("approval_history");
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let path = WorkspacePlan::plan_path(root.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let loaded = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(loaded.approval(), plan.approval());
    assert_eq!(loaded.approval_history().len(), 1);
    assert_eq!(loaded.revision_diff().baseline, RevisionBaseline::Available);
    assert!(loaded.is_implementation_allowed());
}

#[test]
fn revision_legacy_revised_baseline_is_unavailable_but_old_approval_metadata_survives() {
    let (root, mut plan) = fixture();
    let old_approval = plan.approval().unwrap().clone();
    plan.set_title("Already revised").unwrap();
    let mut legacy = serde_json::to_value(&plan).unwrap();
    legacy["schema_version"] = 3.into();
    legacy.as_object_mut().unwrap().remove("approval_history");
    let path = WorkspacePlan::plan_path(root.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let mut loaded = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(
        loaded.revision_diff().baseline,
        RevisionBaseline::Unavailable
    );
    assert_eq!(loaded.revision_diff().affected_task_ids, ["A", "B", "C"]);
    assert!(loaded.approval_history()[0].content.is_none());
    loaded.approve("new-human").unwrap();
    loaded.save(root.path()).unwrap();
    let loaded = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(loaded.approval_history().len(), 2);
    assert_eq!(loaded.approval_history()[0].approval, old_approval);
    assert!(loaded.approval_history()[0].content.is_none());
}

#[test]
fn revision_corrupted_history_is_rejected() {
    let (root, mut plan) = fixture();
    plan.save(root.path()).unwrap();
    let mut bad = serde_json::to_value(&plan).unwrap();
    bad["approval_history"][0]["content"]["title"] = "Corrupt".into();
    std::fs::write(
        WorkspacePlan::plan_path(root.path()),
        serde_json::to_vec(&bad).unwrap(),
    )
    .unwrap();
    assert!(WorkspacePlan::load(root.path()).is_err());
}

#[test]
fn revision_legacy_hash_ambiguity_cannot_hide_a_new_semantic_mutation() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Legacy").unwrap();
    plan.set_requirements(vec![Requirement {
        id: "R".into(),
        description: "Scope".into(),
        acceptance_criteria: vec![],
        priority: Priority::Must,
    }])
    .unwrap();
    plan.set_plan_content("a\0b", "c", vec![]).unwrap();
    plan.approve("human").unwrap();
    let legacy_hash = plan.content_hash();
    plan.set_plan_content("a", "b\0c", vec![]).unwrap();
    assert_eq!(plan.content_hash(), legacy_hash);
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
    assert_eq!(plan.revision_diff().changes.len(), 2);
    assert!(!plan.is_implementation_allowed());
}
