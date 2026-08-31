use esi_workspace_plan::*;
use std::path::PathBuf;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn workspace() -> TempDir {
    TempDir::new().expect("create temp workspace")
}

fn sample_requirements() -> Vec<Requirement> {
    vec![
        Requirement {
            id: "REQ-001".to_string(),
            description: "User can view the 3D map".to_string(),
            acceptance_criteria: vec!["Map renders in browser".to_string()],
            priority: Priority::Must,
        },
        Requirement {
            id: "REQ-002".to_string(),
            description: "User can rotate the camera".to_string(),
            acceptance_criteria: vec!["Drag rotates view".to_string()],
            priority: Priority::Should,
        },
    ]
}

fn sample_tasks() -> Vec<PlannedTask> {
    vec![PlannedTask {
        id: "TASK-001".to_string(),
        title: "Set up WebGL renderer".to_string(),
        description: "Initialize Three.js scene".to_string(),
        status: PlannedTaskStatus::Pending,
    }]
}

fn discovery_plan(workspace: &TempDir) -> WorkspacePlan {
    WorkspacePlan::new(workspace.path(), "3D Map Project").unwrap()
}

fn planning_plan(workspace: &TempDir) -> WorkspacePlan {
    let mut plan = discovery_plan(workspace);
    plan.set_requirements(sample_requirements()).unwrap();
    plan.set_plan_content(
        "Build a 3D map viewer with Three.js",
        "WebGL + TypeScript architecture",
        sample_tasks(),
    )
    .unwrap();
    plan
}

fn approved_plan(workspace: &TempDir) -> WorkspacePlan {
    let mut plan = planning_plan(workspace);
    plan.approve("operator@example.com").unwrap();
    plan
}

// ---------------------------------------------------------------------------
// Model creation tests
// ---------------------------------------------------------------------------

#[test]
fn new_plan_starts_in_discovery() {
    let ws = workspace();
    let plan = discovery_plan(&ws);
    assert_eq!(plan.status(), WorkspacePlanStatus::Discovery);
    assert!(!plan.events().is_empty());
    assert!(matches!(plan.events()[0].event, PlanEventKind::Created));
}

#[test]
fn new_plan_rejects_empty_title() {
    let ws = workspace();
    let result = WorkspacePlan::new(ws.path(), "");
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("title must be non-empty"));
}

#[test]
fn plan_has_workspace_id() {
    let ws = workspace();
    let plan = discovery_plan(&ws);
    assert!(!plan.workspace_id().is_empty());
    assert_eq!(plan.workspace_id().len(), 16);
}

// ---------------------------------------------------------------------------
// FSM transition tests
// ---------------------------------------------------------------------------

#[test]
fn transition_discovery_to_planning() {
    let ws = workspace();
    let plan = planning_plan(&ws);
    assert_eq!(plan.status(), WorkspacePlanStatus::Planning);
}

#[test]
fn transition_planning_to_approved() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    assert_eq!(plan.status(), WorkspacePlanStatus::Approved);
    assert!(plan.approval().is_some());
}

#[test]
fn transition_approved_to_revising_on_content_change() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    assert_eq!(plan.status(), WorkspacePlanStatus::Approved);

    // Modify requirements → should auto-transition to Revising
    plan.set_requirements(vec![Requirement {
        id: "REQ-NEW".to_string(),
        description: "Added requirement".to_string(),
        acceptance_criteria: vec!["Works".to_string()],
        priority: Priority::Must,
    }])
    .unwrap();

    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
    assert!(plan.revision_count() > 0);
}

#[test]
fn changing_an_approved_title_requires_reapproval() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);

    plan.set_title("Revised 3D Map Project").unwrap();

    assert_eq!(plan.title(), "Revised 3D Map Project");
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
    assert!(!plan.is_implementation_allowed());
}

#[test]
fn transition_revising_to_approved() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    plan.set_requirements(vec![Requirement {
        id: "REQ-NEW".to_string(),
        description: "Added requirement".to_string(),
        acceptance_criteria: vec!["Works".to_string()],
        priority: Priority::Must,
    }])
    .unwrap();
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);

    plan.approve("reviewer@example.com").unwrap();
    assert_eq!(plan.status(), WorkspacePlanStatus::Approved);
}

#[test]
fn cannot_approve_from_discovery() {
    let ws = workspace();
    let mut plan = discovery_plan(&ws);
    let result = plan.approve("user@example.com");
    assert!(result.is_err());
}

#[test]
fn cannot_approve_without_requirements() {
    let ws = workspace();
    let mut plan = discovery_plan(&ws);
    // Set plan content without requirements first
    plan.set_plan_content("description", "notes", vec![])
        .unwrap();
    // Still in discovery because no requirements → cannot approve
    let result = plan.approve("user@example.com");
    assert!(result.is_err());
}

#[test]
fn cannot_approve_with_empty_approver() {
    let ws = workspace();
    let mut plan = planning_plan(&ws);
    let result = plan.approve("  ");
    assert!(result.is_err());
}

#[test]
fn explicit_revision_request() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    plan.request_revision("User wants to change scope").unwrap();
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
}

#[test]
fn cannot_request_revision_from_planning() {
    let ws = workspace();
    let mut plan = planning_plan(&ws);
    let result = plan.request_revision("too early");
    assert!(result.is_err());
}

#[test]
fn allowed_transitions() {
    assert!(is_plan_transition_allowed(
        WorkspacePlanStatus::Discovery,
        WorkspacePlanStatus::Planning
    ));
    assert!(is_plan_transition_allowed(
        WorkspacePlanStatus::Planning,
        WorkspacePlanStatus::Approved
    ));
    assert!(is_plan_transition_allowed(
        WorkspacePlanStatus::Approved,
        WorkspacePlanStatus::Revising
    ));
    assert!(is_plan_transition_allowed(
        WorkspacePlanStatus::Revising,
        WorkspacePlanStatus::Approved
    ));
}

#[test]
fn disallowed_transitions() {
    assert!(!is_plan_transition_allowed(
        WorkspacePlanStatus::Discovery,
        WorkspacePlanStatus::Approved
    ));
    assert!(!is_plan_transition_allowed(
        WorkspacePlanStatus::Planning,
        WorkspacePlanStatus::Revising
    ));
    assert!(!is_plan_transition_allowed(
        WorkspacePlanStatus::Revising,
        WorkspacePlanStatus::Discovery
    ));
}

// ---------------------------------------------------------------------------
// Persistence tests
// ---------------------------------------------------------------------------

#[test]
fn save_and_load_round_trip() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    plan.save(ws.path()).unwrap();

    assert!(WorkspacePlan::exists(ws.path()));

    let loaded = WorkspacePlan::load(ws.path()).unwrap().unwrap();
    assert_eq!(loaded.status(), WorkspacePlanStatus::Approved);
    assert_eq!(loaded.title(), plan.title());
    assert_eq!(loaded.requirements().len(), plan.requirements().len());
    assert_eq!(loaded.workspace_id(), plan.workspace_id());
    assert!(loaded.approval().is_some());
}

#[test]
fn load_nonexistent_returns_none() {
    let ws = workspace();
    let result = WorkspacePlan::load(ws.path()).unwrap();
    assert!(result.is_none());
}

#[test]
fn exists_returns_false_for_empty_workspace() {
    let ws = workspace();
    assert!(!WorkspacePlan::exists(ws.path()));
}

#[test]
fn save_creates_esi_directory() {
    let ws = workspace();
    let plan = discovery_plan(&ws);
    plan.save(ws.path()).unwrap();
    assert!(ws.path().join(".esi").is_dir());
    assert!(ws.path().join(".esi/workspace-plan.json").is_file());
}

// ---------------------------------------------------------------------------
// Content hash integrity tests
// ---------------------------------------------------------------------------

#[test]
fn content_hash_changes_on_requirement_modification() {
    let ws = workspace();
    let mut plan = planning_plan(&ws);
    let hash_before = plan.content_hash();

    plan.set_requirements(vec![Requirement {
        id: "REQ-DIFFERENT".to_string(),
        description: "Different requirement".to_string(),
        acceptance_criteria: vec!["Different criteria".to_string()],
        priority: Priority::Could,
    }])
    .unwrap();

    let hash_after = plan.content_hash();
    assert_ne!(hash_before, hash_after);
}

#[test]
fn content_hash_changes_on_description_modification() {
    let ws = workspace();
    let mut plan = planning_plan(&ws);
    let hash_before = plan.content_hash();

    plan.set_plan_content("Completely different plan", "New arch", vec![])
        .unwrap();

    let hash_after = plan.content_hash();
    assert_ne!(hash_before, hash_after);
}

#[test]
fn approval_hash_matches_at_approval_time() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    let approval = plan.approval().unwrap();
    assert_eq!(approval.content_hash, plan.content_hash());
}

#[test]
fn approval_invalidated_after_content_change() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    assert!(plan.is_implementation_allowed());

    plan.set_plan_content("Modified after approval", "Changed arch", vec![])
        .unwrap();

    assert!(!plan.is_implementation_allowed());
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
}

// ---------------------------------------------------------------------------
// Implementation gate tests
// ---------------------------------------------------------------------------

#[test]
fn gate_blocks_discovery_status() {
    let ws = workspace();
    let plan = discovery_plan(&ws);
    let result = plan.require_approved_plan();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("implementation blocked"));
}

#[test]
fn gate_blocks_planning_status() {
    let ws = workspace();
    let plan = planning_plan(&ws);
    let result = plan.require_approved_plan();
    assert!(result.is_err());
}

#[test]
fn gate_allows_approved_status() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    plan.require_approved_plan().unwrap();
}

#[test]
fn gate_blocks_revising_status() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    plan.request_revision("scope change").unwrap();
    let result = plan.require_approved_plan();
    assert!(result.is_err());
}

#[test]
fn gate_blocks_after_content_change_invalidation() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    plan.set_requirements(vec![Requirement {
        id: "REQ-X".to_string(),
        description: "Extra".to_string(),
        acceptance_criteria: vec!["Done".to_string()],
        priority: Priority::Must,
    }])
    .unwrap();
    assert!(plan.require_approved_plan().is_err());
}

// ---------------------------------------------------------------------------
// Standalone gate function tests
// ---------------------------------------------------------------------------

#[test]
fn standalone_gate_blocks_no_plan() {
    let ws = workspace();
    let result = check_workspace_plan_gate(ws.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("no workspace plan exists"));
}

#[test]
fn standalone_gate_blocks_unapproved_plan() {
    let ws = workspace();
    let plan = planning_plan(&ws);
    plan.save(ws.path()).unwrap();

    let result = check_workspace_plan_gate(ws.path());
    assert!(result.is_err());
}

#[test]
fn standalone_gate_allows_approved_plan() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    plan.save(ws.path()).unwrap();

    check_workspace_plan_gate(ws.path()).unwrap();
}

#[test]
fn standalone_gate_blocks_after_persisted_revision() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    plan.request_revision("scope change").unwrap();
    plan.save(ws.path()).unwrap();

    let result = check_workspace_plan_gate(ws.path());
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Innovation discovery tests
// ---------------------------------------------------------------------------

#[test]
fn innovation_discovery_stored_in_plan() {
    let ws = workspace();
    let mut plan = discovery_plan(&ws);
    plan.set_innovation_discovery(InnovationDiscovery {
        brief: "Explore WebXR for immersive maps".to_string(),
        research_findings: vec!["WebXR supported in Chrome".to_string()],
        candidates: vec!["Three.js".to_string(), "Babylon.js".to_string()],
        selected_rationale: "Three.js has better community support".to_string(),
    })
    .unwrap();

    assert!(plan.innovation_discovery().is_some());
    let discovery = plan.innovation_discovery().unwrap();
    assert_eq!(discovery.candidates.len(), 2);
}

#[test]
fn innovation_discovery_included_in_content_hash() {
    let ws = workspace();
    let mut plan = planning_plan(&ws);
    let hash_before = plan.content_hash();

    plan.set_innovation_discovery(InnovationDiscovery {
        brief: "Research phase output".to_string(),
        research_findings: vec!["finding 1".to_string()],
        candidates: vec![],
        selected_rationale: String::new(),
    })
    .unwrap();

    assert_ne!(hash_before, plan.content_hash());
}

#[test]
fn innovation_discovery_change_invalidates_approval() {
    let ws = workspace();
    let mut plan = approved_plan(&ws);
    assert!(plan.is_implementation_allowed());

    plan.set_innovation_discovery(InnovationDiscovery {
        brief: "New research added post-approval".to_string(),
        research_findings: vec![],
        candidates: vec![],
        selected_rationale: String::new(),
    })
    .unwrap();

    assert!(!plan.is_implementation_allowed());
    assert_eq!(plan.status(), WorkspacePlanStatus::Revising);
}

#[test]
fn innovation_rejects_empty_brief() {
    let ws = workspace();
    let mut plan = discovery_plan(&ws);
    let result = plan.set_innovation_discovery(InnovationDiscovery {
        brief: "".to_string(),
        research_findings: vec![],
        candidates: vec![],
        selected_rationale: String::new(),
    });
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Event history tests
// ---------------------------------------------------------------------------

#[test]
fn events_are_sequentially_numbered() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    for (index, event) in plan.events().iter().enumerate() {
        assert_eq!(event.sequence, index as u64 + 1);
    }
}

#[test]
fn events_contain_all_transitions() {
    let ws = workspace();
    let plan = approved_plan(&ws);
    let transition_events: Vec<_> = plan
        .events()
        .iter()
        .filter(|e| matches!(e.event, PlanEventKind::StatusTransition { .. }))
        .collect();
    // Discovery → Planning, Planning → Approved
    assert_eq!(transition_events.len(), 2);
}

// ---------------------------------------------------------------------------
// Display message tests
// ---------------------------------------------------------------------------

#[test]
fn status_display_messages_are_human_readable() {
    assert!(WorkspacePlanStatus::Discovery
        .display_message()
        .contains("requirements"));
    assert!(WorkspacePlanStatus::Planning
        .display_message()
        .contains("plan"));
    assert!(WorkspacePlanStatus::Approved
        .display_message()
        .contains("approved"));
    assert!(WorkspacePlanStatus::Revising
        .display_message()
        .contains("re-approval"));
}

// ---------------------------------------------------------------------------
// Plan path resolution
// ---------------------------------------------------------------------------

#[test]
fn plan_path_resolves_correctly() {
    let path = WorkspacePlan::plan_path("/home/user/projects/3dmap");
    assert_eq!(
        path,
        PathBuf::from("/home/user/projects/3dmap/.esi/workspace-plan.json")
    );
}

// ---------------------------------------------------------------------------
// Wiki memory-capture outbox (TASK-POST-122)
// ---------------------------------------------------------------------------

#[test]
fn new_plan_has_no_memory_sync_record() {
    let workspace = workspace();
    let plan = discovery_plan(&workspace);
    assert!(plan.memory_sync().is_none());
    assert!(!plan.memory_already_synced());
}

#[test]
fn recording_synced_outcome_matches_current_content_hash() {
    let workspace = workspace();
    let mut plan = approved_plan(&workspace);
    let hash_at_approval = plan.content_hash();

    plan.record_memory_sync(MemorySyncOutcome::Synced);

    let record = plan.memory_sync().expect("memory sync record recorded");
    assert_eq!(record.content_hash, hash_at_approval);
    assert_eq!(record.outcome, MemorySyncOutcome::Synced);
    assert!(plan.memory_already_synced());
}

#[test]
fn recording_pending_outcome_does_not_count_as_synced() {
    let workspace = workspace();
    let mut plan = approved_plan(&workspace);

    plan.record_memory_sync(MemorySyncOutcome::Pending {
        reason: "wiki unreachable".to_string(),
    });

    let record = plan.memory_sync().expect("memory sync record recorded");
    assert_eq!(
        record.outcome,
        MemorySyncOutcome::Pending {
            reason: "wiki unreachable".to_string()
        }
    );
    assert!(!plan.memory_already_synced());
}

#[test]
fn memory_sync_record_is_not_part_of_the_content_hash() {
    let workspace = workspace();
    let mut plan = approved_plan(&workspace);
    let hash_before = plan.content_hash();

    plan.record_memory_sync(MemorySyncOutcome::Synced);

    assert_eq!(plan.content_hash(), hash_before);
    assert!(plan.is_implementation_allowed());
}

#[test]
fn revised_and_reapproved_plan_requires_a_fresh_memory_sync() {
    let workspace = workspace();
    let mut plan = approved_plan(&workspace);
    plan.record_memory_sync(MemorySyncOutcome::Synced);
    assert!(plan.memory_already_synced());

    // Changing approved content moves the plan to Revising and invalidates
    // the previous approval hash; the prior sync record no longer matches
    // the new content hash, so a caller must attempt capture again.
    plan.set_plan_content(
        "Revised 3D map viewer scope",
        "WebGL + TypeScript architecture",
        sample_tasks(),
    )
    .unwrap();
    assert!(!plan.memory_already_synced());

    plan.approve("operator@example.com").unwrap();
    assert!(!plan.memory_already_synced());
}

#[test]
fn memory_sync_state_survives_save_and_load_round_trip() {
    let workspace = workspace();
    let mut plan = approved_plan(&workspace);
    plan.record_memory_sync(MemorySyncOutcome::Synced);
    plan.save(workspace.path()).unwrap();

    let loaded = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
    assert!(loaded.memory_already_synced());
    assert_eq!(
        loaded.memory_sync().unwrap().outcome,
        MemorySyncOutcome::Synced
    );
}

#[test]
fn plans_persisted_before_memory_sync_existed_still_load() {
    // Simulate a plan file written before TASK-POST-122 added the
    // `memory_sync` field: the JSON simply omits the key.
    let workspace = workspace();
    let plan = approved_plan(&workspace);
    let mut value = serde_json::to_value(&plan).unwrap();
    value.as_object_mut().unwrap().remove("memory_sync");
    std::fs::create_dir_all(workspace.path().join(".esi")).unwrap();
    std::fs::write(
        WorkspacePlan::plan_path(workspace.path()),
        serde_json::to_vec_pretty(&value).unwrap(),
    )
    .unwrap();

    let loaded = WorkspacePlan::load(workspace.path()).unwrap().unwrap();
    assert!(loaded.memory_sync().is_none());
    assert!(loaded.is_implementation_allowed());
}
