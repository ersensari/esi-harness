use crate::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

impl WorkspacePlan {
    pub fn content_snapshot(&self) -> PlanContentSnapshot {
        PlanContentSnapshot {
            title: self.title.clone(),
            description: self.description.clone(),
            architecture_notes: self.architecture_notes.clone(),
            requirements: self.requirements.clone(),
            tasks: self
                .tasks
                .iter()
                .map(|task| PlanTaskContent {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    description: task.description.clone(),
                })
                .collect(),
            task_contracts: self.task_contracts.clone(),
            innovation_discovery: self.innovation_discovery.clone(),
        }
    }

    pub fn approval_history(&self) -> &[ApprovedPlanRevision] {
        &self.approval_history
    }

    pub(crate) fn validate_approval_history(&self) -> Result<(), WorkspacePlanError> {
        for revision in &self.approval_history {
            let Some(content) = &revision.content else {
                continue;
            };
            if crate::engine::compute_snapshot_hash(content) != revision.approval.content_hash {
                return Err(WorkspacePlanError::InvalidPersistedPlan);
            }
            let tasks: Vec<_> = content
                .tasks
                .iter()
                .map(|task| PlannedTask {
                    id: task.id.clone(),
                    title: task.title.clone(),
                    description: task.description.clone(),
                    status: PlannedTaskStatus::Pending,
                })
                .collect();
            crate::task_contract::validate(&content.requirements, &tasks, &content.task_contracts)?;
        }
        if let Some(last) = self.approval_history.last() {
            if self.approval.as_ref() != Some(&last.approval) {
                return Err(WorkspacePlanError::InvalidPersistedPlan);
            }
            if self.status == WorkspacePlanStatus::Approved && last.content.is_none() {
                return Err(WorkspacePlanError::InvalidPersistedPlan);
            }
        } else if self.status == WorkspacePlanStatus::Approved {
            return Err(WorkspacePlanError::InvalidPersistedPlan);
        }
        Ok(())
    }

    pub fn revision_diff(&self) -> PlanRevisionDiff {
        let current = self.content_snapshot();
        let Some((previous, previous_content)) = self
            .approval_history
            .last()
            .and_then(|revision| revision.content.as_ref().map(|content| (revision, content)))
        else {
            return PlanRevisionDiff {
                baseline: if self.approval.is_some() {
                    RevisionBaseline::Unavailable
                } else {
                    RevisionBaseline::NoApproval
                },
                approved_hash: self
                    .approval
                    .as_ref()
                    .map(|approval| approval.content_hash.clone()),
                current_hash: self.content_hash(),
                changes: Vec::new(),
                affected_task_ids: self
                    .tasks
                    .iter()
                    .map(|task| task.id.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            };
        };
        let mut changes = Vec::new();
        compare(
            "",
            Some(&diff_value(previous_content)),
            Some(&diff_value(&current)),
            &mut changes,
        );
        PlanRevisionDiff {
            baseline: RevisionBaseline::Available,
            approved_hash: Some(previous.approval.content_hash.clone()),
            current_hash: self.content_hash(),
            affected_task_ids: affected_tasks(previous_content, &current),
            changes,
        }
    }
}

fn diff_value(content: &PlanContentSnapshot) -> Value {
    json!({
        "title": content.title, "description": content.description, "architecture_notes": content.architecture_notes,
        "requirements": content.requirements.iter().map(|requirement| (&requirement.id, requirement)).collect::<BTreeMap<_, _>>(),
        "requirement_order": content.requirements.iter().map(|requirement| &requirement.id).collect::<Vec<_>>(),
        "tasks": content.tasks.iter().map(|task| (&task.id, task)).collect::<BTreeMap<_, _>>(),
        "task_order": content.tasks.iter().map(|task| &task.id).collect::<Vec<_>>(),
        "task_contracts": content.task_contracts, "innovation_discovery": content.innovation_discovery,
    })
}

fn compare(
    path: &str,
    before: Option<&Value>,
    after: Option<&Value>,
    changes: &mut Vec<PlanChange>,
) {
    if before == after {
        return;
    }
    if let (Some(Value::Object(left)), Some(Value::Object(right))) = (before, after) {
        let keys: BTreeSet<_> = left.keys().chain(right.keys()).collect();
        for key in keys {
            let escaped = key.replace('~', "~0").replace('/', "~1");
            compare(
                &format!("{path}/{escaped}"),
                left.get(key),
                right.get(key),
                changes,
            );
        }
    } else {
        changes.push(PlanChange {
            path: path.into(),
            before: before.cloned(),
            after: after.cloned(),
        });
    }
}

fn affected_tasks(before: &PlanContentSnapshot, after: &PlanContentSnapshot) -> Vec<String> {
    let old_tasks: BTreeMap<_, _> = before
        .tasks
        .iter()
        .map(|task| (task.id.clone(), task))
        .collect();
    let new_tasks: BTreeMap<_, _> = after
        .tasks
        .iter()
        .map(|task| (task.id.clone(), task))
        .collect();
    let all: BTreeSet<String> = old_tasks.keys().chain(new_tasks.keys()).cloned().collect();
    let old_requirements: BTreeMap<_, _> = before
        .requirements
        .iter()
        .map(|item| (&item.id, item))
        .collect();
    let new_requirements: BTreeMap<_, _> = after
        .requirements
        .iter()
        .map(|item| (&item.id, item))
        .collect();
    let changed_requirements: BTreeSet<_> = old_requirements
        .keys()
        .chain(new_requirements.keys())
        .filter(|id| old_requirements.get(*id) != new_requirements.get(*id))
        .map(|id| id.as_str())
        .collect();
    let task_reorder = old_tasks.keys().eq(new_tasks.keys())
        && before
            .tasks
            .iter()
            .map(|task| &task.id)
            .ne(after.tasks.iter().map(|task| &task.id));
    let requirement_reorder = old_requirements.keys().eq(new_requirements.keys())
        && before
            .requirements
            .iter()
            .map(|item| &item.id)
            .ne(after.requirements.iter().map(|item| &item.id));
    let referenced: BTreeSet<_> = before
        .task_contracts
        .values()
        .chain(after.task_contracts.values())
        .flat_map(|contract| {
            contract
                .acceptance_criteria
                .iter()
                .filter_map(|criterion| criterion.requirement_id.as_deref())
        })
        .collect();
    if before.title != after.title
        || before.description != after.description
        || before.architecture_notes != after.architecture_notes
        || before.innovation_discovery != after.innovation_discovery
        || task_reorder
        || requirement_reorder
        || changed_requirements
            .iter()
            .any(|id| !referenced.contains(id))
    {
        return all.into_iter().collect();
    }
    let mut affected: BTreeSet<String> = all
        .iter()
        .filter(|id| {
            old_tasks.get(*id) != new_tasks.get(*id)
                || before.task_contracts.get(*id) != after.task_contracts.get(*id)
        })
        .cloned()
        .collect();
    if !changed_requirements.is_empty() {
        for id in &all {
            for contracts in [&before.task_contracts, &after.task_contracts] {
                let relevant = contracts.get(id).is_none_or(|contract| {
                    contract.acceptance_criteria.is_empty()
                        || contract.acceptance_criteria.iter().any(|criterion| {
                            criterion
                                .requirement_id
                                .as_deref()
                                .is_none_or(|requirement| {
                                    changed_requirements.contains(requirement)
                                })
                        })
                });
                if relevant {
                    affected.insert(id.clone());
                }
            }
        }
    }
    let mut consumers: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for contracts in [&before.task_contracts, &after.task_contracts] {
        for (task, contract) in contracts {
            for dependency in &contract.depends_on {
                consumers
                    .entry(dependency.clone())
                    .or_default()
                    .push(task.clone());
            }
        }
    }
    let mut queue: VecDeque<_> = affected.iter().cloned().collect();
    while let Some(task) = queue.pop_front() {
        if let Some(dependents) = consumers.get(&task) {
            for dependent in dependents {
                if affected.insert(dependent.clone()) {
                    queue.push_back(dependent.clone());
                }
            }
        }
    }
    affected.into_iter().collect()
}
