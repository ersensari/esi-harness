use crate::*;
use std::collections::BTreeMap;

impl WorkspacePlan {
    /// Seed an un-authored plan only; re-opening/retrying never replaces existing work.
    pub fn apply_template(
        &mut self,
        template: PlanningTemplate,
        objective: &str,
    ) -> Result<bool, WorkspacePlanError> {
        if objective.trim().is_empty() {
            return Err(WorkspacePlanError::InvalidInput(
                "template objective must be non-empty".into(),
            ));
        }
        if !self.tasks.is_empty() || !self.description.is_empty() || self.approval.is_some() {
            return Ok(false);
        }
        let mut next = self.clone();
        if next.requirements.is_empty() {
            next.set_requirements(vec![Requirement {
                id: "REQ-001".into(),
                description: objective.into(),
                acceptance_criteria: vec![format!("Requested outcome is observable: {objective}")],
                priority: Priority::Must,
            }])?;
        }
        let titles: &[&str] = match template {
            PlanningTemplate::SmallChange => &[
                "Implement scoped change",
                "Validate changed behavior",
                "Review and hand off",
            ],
            PlanningTemplate::Greenfield => &[
                "Design project boundaries",
                "Prepare project foundation",
                "Implement required behavior",
                "Validate end-to-end behavior",
                "Review and hand off",
            ],
        };
        let mut tasks = Vec::new();
        let mut contracts = BTreeMap::new();
        for (index, title) in titles.iter().enumerate() {
            let id = format!("TASK-{:03}", index + 1);
            let criteria: Vec<_> = next
                .requirements
                .iter()
                .flat_map(|requirement| {
                    let descriptions = if requirement.acceptance_criteria.is_empty() {
                        vec![requirement.description.clone()]
                    } else {
                        requirement.acceptance_criteria.clone()
                    };
                    descriptions
                        .into_iter()
                        .map(move |description| (requirement.id.clone(), description))
                })
                .enumerate()
                .map(
                    |(slot, (requirement_id, description))| TaskAcceptanceCriterion {
                        id: format!("AC-{:03}", slot + 1),
                        description: format!("{title}: {description}"),
                        requirement_id: Some(requirement_id),
                    },
                )
                .collect();
            let expectation = TaskValidationExpectation {
                id: "VALIDATE-001".into(),
                description: "Record task-appropriate repository-native checks and their actual results; missing checks are not PASS.".into(),
                criterion_ids: criteria.iter().map(|criterion| criterion.id.clone()).collect(),
            };
            contracts.insert(
                id.clone(),
                TaskContract {
                    depends_on: if index == 0 {
                        vec![]
                    } else {
                        vec![format!("TASK-{index:03}")]
                    },
                    affected_components: vec![],
                    acceptance_criteria: criteria,
                    validation_expectations: vec![expectation],
                },
            );
            tasks.push(PlannedTask {
                id,
                title: (*title).into(),
                description: format!("{title} for: {objective}"),
                status: PlannedTaskStatus::Pending,
            });
        }
        let architecture = if next.architecture_notes.is_empty() {
            match template {
                PlanningTemplate::SmallChange => "Reuse existing architecture; limit the change to the requested behavior.",
                PlanningTemplate::Greenfield => "Identify entry points, component boundaries and repository-native validation before implementation.",
            }.to_string()
        } else {
            next.architecture_notes.clone()
        };
        next.set_plan_content(objective, architecture, tasks)?;
        next.set_task_contracts(contracts)?;
        *self = next;
        Ok(true)
    }
}
