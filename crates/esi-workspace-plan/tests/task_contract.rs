use esi_workspace_plan::*;
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, WorkspacePlan) {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Contract fixture").unwrap();
    plan.set_requirements(vec![Requirement {
        id: "R1".into(),
        description: "Reliable work".into(),
        acceptance_criteria: vec!["Tests pass".into()],
        priority: Priority::Must,
    }])
    .unwrap();
    let tasks = ["B", "A", "C"]
        .into_iter()
        .map(|id| PlannedTask {
            id: id.into(),
            title: id.into(),
            description: "Implement slice".into(),
            status: PlannedTaskStatus::Pending,
        })
        .collect();
    plan.set_plan_content("Build it", "Local", tasks).unwrap();
    (root, plan)
}

fn contract(dependency: &str) -> TaskContract {
    TaskContract {
        depends_on: if dependency.is_empty() {
            vec![]
        } else {
            vec![dependency.into()]
        },
        affected_components: vec!["engine".into()],
        acceptance_criteria: vec![TaskAcceptanceCriterion {
            id: "AC1".into(),
            description: "Behavior passes".into(),
            requirement_id: Some("R1".into()),
        }],
        validation_expectations: vec![TaskValidationExpectation {
            id: "V1".into(),
            description: "Run targeted fixture".into(),
            criterion_ids: vec!["AC1".into()],
        }],
    }
}

#[test]
fn task_contract_orders_dependencies_deterministically_and_persists() {
    let (root, mut plan) = fixture();
    plan.set_task_contracts(BTreeMap::from([
        ("B".into(), contract("A")),
        ("C".into(), contract("B")),
    ]))
    .unwrap();
    assert_eq!(plan.task_execution_order().unwrap(), ["A", "B", "C"]);
    plan.approve("fixture-human").unwrap();
    plan.save(root.path()).unwrap();
    let loaded = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(loaded.task_contracts(), plan.task_contracts());
    assert!(loaded.is_implementation_allowed());
}

#[test]
fn task_contract_cycles_missing_self_and_duplicate_dependencies_are_atomic_errors() {
    let (_root, mut plan) = fixture();
    plan.approve("fixture-human").unwrap();
    let original = serde_json::to_value(&plan).unwrap();
    let mut duplicate = contract("A");
    duplicate.depends_on.push("A".into());
    for contracts in [
        BTreeMap::from([("A".into(), contract("B")), ("B".into(), contract("A"))]),
        BTreeMap::from([("A".into(), contract("missing"))]),
        BTreeMap::from([("A".into(), contract("A"))]),
        BTreeMap::from([("B".into(), duplicate)]),
        BTreeMap::from([("missing".into(), contract("A"))]),
    ] {
        assert!(plan.set_task_contracts(contracts).is_err());
        assert_eq!(serde_json::to_value(&plan).unwrap(), original);
    }
}

#[test]
fn task_contract_rejects_duplicate_task_requirement_criterion_and_validation_ids() {
    let (_root, mut plan) = fixture();
    let mut tasks = plan.tasks().to_vec();
    tasks.push(tasks[0].clone());
    assert!(plan
        .set_plan_content("description", "architecture", tasks)
        .is_err());
    let requirement = plan.requirements()[0].clone();
    assert!(plan
        .set_requirements(vec![requirement.clone(), requirement])
        .is_err());
    let mut criteria = contract("");
    criteria
        .acceptance_criteria
        .push(criteria.acceptance_criteria[0].clone());
    let mut validations = contract("");
    validations
        .validation_expectations
        .push(validations.validation_expectations[0].clone());
    for invalid in [criteria, validations] {
        assert!(plan
            .set_task_contracts(BTreeMap::from([("A".into(), invalid)]))
            .is_err());
    }
}

#[test]
fn task_contract_references_and_empty_descriptions_are_validated() {
    let (_root, mut plan) = fixture();
    let mut requirement = contract("");
    requirement.acceptance_criteria[0].requirement_id = Some("missing".into());
    let mut criterion = contract("");
    criterion.validation_expectations[0].criterion_ids = vec!["missing".into()];
    let mut description = contract("");
    description.validation_expectations[0].description.clear();
    let mut empty = contract("");
    empty.validation_expectations[0].criterion_ids.clear();
    for invalid in [requirement, criterion, description, empty] {
        assert!(plan
            .set_task_contracts(BTreeMap::from([("A".into(), invalid)]))
            .is_err());
    }
}

#[test]
fn task_contract_scope_changes_revoke_but_status_and_outbox_do_not() {
    let (_root, mut plan) = fixture();
    plan.set_task_contracts(BTreeMap::from([("B".into(), contract("A"))]))
        .unwrap();
    plan.approve("fixture-human").unwrap();
    let hash = plan.content_hash();
    let mut tasks = plan.tasks().to_vec();
    tasks[0].status = PlannedTaskStatus::Completed;
    plan.set_plan_content("Build it", "Local", tasks).unwrap();
    plan.record_memory_sync(MemorySyncOutcome::Synced);
    assert_eq!(plan.content_hash(), hash);
    assert!(plan.is_implementation_allowed());
    let mut changed = contract("C");
    changed.affected_components.push("UI".into());
    plan.set_task_contracts(BTreeMap::from([("B".into(), changed)]))
        .unwrap();
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
    assert_ne!(plan.content_hash(), hash);
    assert!(!plan.is_implementation_allowed());
}

#[test]
fn task_contract_removing_referenced_content_fails_without_partial_change() {
    let (_root, mut plan) = fixture();
    plan.set_task_contracts(BTreeMap::from([("B".into(), contract("A"))]))
        .unwrap();
    let before = serde_json::to_value(&plan).unwrap();
    assert!(plan.set_plan_content("changed", "changed", vec![]).is_err());
    let mut requirement = plan.requirements()[0].clone();
    requirement.id = "R2".into();
    assert!(plan.set_requirements(vec![requirement]).is_err());
    assert_eq!(serde_json::to_value(&plan).unwrap(), before);
}

#[test]
fn task_contract_load_rejects_corrupt_graph_and_legacy_smuggling() {
    let (root, mut plan) = fixture();
    plan.set_task_contracts(BTreeMap::from([("B".into(), contract("A"))]))
        .unwrap();
    plan.save(root.path()).unwrap();
    let path = WorkspacePlan::plan_path(root.path());
    let original = serde_json::to_value(&plan).unwrap();
    let mut corrupt = original.clone();
    corrupt["task_contracts"]["B"]["depends_on"] = serde_json::json!(["missing"]);
    std::fs::write(&path, serde_json::to_vec(&corrupt).unwrap()).unwrap();
    assert!(WorkspacePlan::load(root.path()).is_err());
    let mut legacy = original;
    legacy["schema_version"] = 2.into();
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(WorkspacePlan::load(root.path()).is_err());
}

#[test]
fn task_contract_schema_two_migration_preserves_approval_without_writing() {
    let (root, mut plan) = fixture();
    plan.approve("fixture-human").unwrap();
    let hash = plan.content_hash();
    let mut legacy = serde_json::to_value(&plan).unwrap();
    legacy["schema_version"] = 2.into();
    let bytes = serde_json::to_vec(&legacy).unwrap();
    let path = WorkspacePlan::plan_path(root.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut loaded = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(loaded.content_hash(), hash);
    assert!(loaded.is_implementation_allowed());
    loaded.save(root.path()).unwrap();
    assert!(WorkspacePlan::load(root.path())
        .unwrap()
        .unwrap()
        .is_implementation_allowed());
}

#[test]
fn task_contract_hash_covers_each_semantic_field_and_avoids_delimiter_ambiguity() {
    let (_root, mut plan) = fixture();
    plan.set_task_contracts(BTreeMap::from([("A".into(), contract(""))]))
        .unwrap();
    let hash = plan.content_hash();
    for field in 0..4 {
        let mut changed = contract("");
        match field {
            0 => changed.acceptance_criteria[0].description.push('!'),
            1 => changed.validation_expectations[0].description.push('!'),
            2 => changed.affected_components.push("extra".into()),
            _ => changed.acceptance_criteria[0].requirement_id = None,
        }
        let mut copy = plan.clone();
        copy.set_task_contracts(BTreeMap::from([("A".into(), changed)]))
            .unwrap();
        assert_ne!(copy.content_hash(), hash);
    }
    let mut a = plan.clone();
    let mut b = plan;
    a.set_title("a\0b").unwrap();
    a.set_plan_content("c", "Local", a.tasks().to_vec())
        .unwrap();
    b.set_title("a").unwrap();
    b.set_plan_content("b\0c", "Local", b.tasks().to_vec())
        .unwrap();
    assert_ne!(a.content_hash(), b.content_hash());
}
