use esi_workspace_plan::*;

#[test]
fn template_small_change_is_three_pending_tasks_with_ordered_contracts() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Fix button").unwrap();
    assert!(plan
        .apply_template(PlanningTemplate::SmallChange, "Fix button click")
        .unwrap());
    assert_eq!(plan.tasks().len(), 3);
    assert_eq!(plan.task_contracts().len(), 3);
    assert_eq!(
        plan.task_execution_order().unwrap(),
        ["TASK-001", "TASK-002", "TASK-003"]
    );
    assert!(plan
        .tasks()
        .iter()
        .all(|task| task.status == PlannedTaskStatus::Pending));
    assert_eq!(plan.status(), WorkspacePlanStatus::Planning);
    assert!(!plan.is_implementation_allowed());
}

#[test]
fn template_greenfield_is_five_tasks_and_preserves_existing_requirements_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Existing title").unwrap();
    let requirements = vec![Requirement {
        id: "REQ-real".into(),
        description: "Keep user scope".into(),
        acceptance_criteria: vec!["User criterion unchanged".into()],
        priority: Priority::Should,
    }];
    plan.set_requirements(requirements.clone()).unwrap();
    assert!(plan
        .apply_template(PlanningTemplate::Greenfield, "New project")
        .unwrap());
    assert_eq!(plan.tasks().len(), 5);
    assert_eq!(plan.requirements(), requirements);
    assert!(plan
        .task_contracts()
        .values()
        .all(|contract| contract.acceptance_criteria[0]
            .description
            .contains("User criterion unchanged")));
    plan.save(root.path()).unwrap();
    let bytes = std::fs::read(WorkspacePlan::plan_path(root.path())).unwrap();
    let mut reopened = WorkspacePlan::load(root.path()).unwrap().unwrap();
    assert!(!reopened
        .apply_template(PlanningTemplate::SmallChange, "Replace everything")
        .unwrap());
    assert_eq!(reopened.requirements(), requirements);
    assert_eq!(reopened.tasks().len(), 5);
    assert_eq!(
        std::fs::read(WorkspacePlan::plan_path(root.path())).unwrap(),
        bytes
    );
}

#[test]
fn template_never_replaces_approved_content_or_accepts_empty_objective() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Scoped change").unwrap();
    let original = serde_json::to_value(&plan).unwrap();
    assert!(plan
        .apply_template(PlanningTemplate::SmallChange, " ")
        .is_err());
    assert_eq!(serde_json::to_value(&plan).unwrap(), original);
    plan.apply_template(PlanningTemplate::SmallChange, "Change")
        .unwrap();
    plan.approve("fixture-human").unwrap();
    let approved = serde_json::to_value(&plan).unwrap();
    assert!(!plan
        .apply_template(PlanningTemplate::Greenfield, "Different")
        .unwrap());
    assert_eq!(serde_json::to_value(&plan).unwrap(), approved);
}

#[test]
fn template_scope_can_be_replaced_atomically_without_transient_dangling_references() {
    let root = tempfile::tempdir().unwrap();
    let mut plan = WorkspacePlan::new(root.path(), "Scope").unwrap();
    plan.apply_template(PlanningTemplate::SmallChange, "Change")
        .unwrap();
    plan.approve("fixture-human").unwrap();
    let hash = plan.content_hash();
    plan.replace_task_scope(
        plan.requirements().to_vec(),
        plan.tasks().to_vec(),
        plan.task_contracts().clone(),
    )
    .unwrap();
    assert!(plan.is_implementation_allowed());
    assert_eq!(plan.content_hash(), hash);
    let mut requirements = plan.requirements().to_vec();
    requirements[0].id = "renamed".into();
    let mut contracts = plan.task_contracts().clone();
    for contract in contracts.values_mut() {
        contract.acceptance_criteria[0].requirement_id = Some("renamed".into());
    }
    plan.replace_task_scope(requirements, plan.tasks().to_vec(), contracts)
        .unwrap();
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
    assert_ne!(plan.content_hash(), hash);
}
