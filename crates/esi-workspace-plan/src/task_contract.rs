use crate::{PlannedTask, Requirement, TaskContract, WorkspacePlanError};
use std::collections::{BTreeMap, BTreeSet};

fn invalid(message: impl Into<String>) -> WorkspacePlanError {
    WorkspacePlanError::InvalidInput(message.into())
}

fn unique<'a>(
    values: impl IntoIterator<Item = &'a str>,
    kind: &str,
) -> Result<BTreeSet<&'a str>, WorkspacePlanError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.trim().is_empty() || !seen.insert(value) {
            return Err(invalid(format!("empty or duplicate {kind}: {value:?}")));
        }
    }
    Ok(seen)
}

pub(crate) fn validate(
    requirements: &[Requirement],
    tasks: &[PlannedTask],
    contracts: &BTreeMap<String, TaskContract>,
) -> Result<Vec<String>, WorkspacePlanError> {
    let requirement_ids = unique(requirements.iter().map(|r| r.id.as_str()), "requirement id")?;
    let task_ids = unique(tasks.iter().map(|t| t.id.as_str()), "task id")?;
    if tasks.iter().any(|task| task.title.trim().is_empty()) {
        return Err(invalid("task title must be non-empty"));
    }
    let mut indegree: BTreeMap<&str, usize> = task_ids.iter().map(|id| (*id, 0)).collect();
    let mut consumers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (task, contract) in contracts {
        if !task_ids.contains(task.as_str()) {
            return Err(invalid(format!("contract references missing task: {task}")));
        }
        let dependencies = unique(contract.depends_on.iter().map(String::as_str), "dependency")?;
        for dependency in &dependencies {
            if *dependency == task || !task_ids.contains(dependency) {
                return Err(invalid(format!(
                    "invalid dependency {dependency} for {task}"
                )));
            }
            consumers.entry(dependency).or_default().push(task);
        }
        indegree.insert(task, dependencies.len());
        unique(
            contract.affected_components.iter().map(String::as_str),
            "component",
        )?;
        let criteria = unique(
            contract.acceptance_criteria.iter().map(|c| c.id.as_str()),
            "criterion id",
        )?;
        for criterion in &contract.acceptance_criteria {
            if criterion.description.trim().is_empty()
                || criterion
                    .requirement_id
                    .as_ref()
                    .is_some_and(|id| !requirement_ids.contains(id.as_str()))
            {
                return Err(invalid(format!(
                    "invalid criterion {} for {task}",
                    criterion.id
                )));
            }
        }
        unique(
            contract
                .validation_expectations
                .iter()
                .map(|v| v.id.as_str()),
            "validation id",
        )?;
        for expectation in &contract.validation_expectations {
            let references = unique(
                expectation.criterion_ids.iter().map(String::as_str),
                "criterion reference",
            )?;
            if expectation.description.trim().is_empty()
                || references.is_empty()
                || references.iter().any(|id| !criteria.contains(id))
            {
                return Err(invalid(format!(
                    "invalid validation expectation {} for {task}",
                    expectation.id
                )));
            }
        }
    }
    let mut ready: BTreeSet<&str> = indegree
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect();
    let mut order = Vec::with_capacity(tasks.len());
    while let Some(id) = ready.pop_first() {
        order.push(id.to_string());
        if let Some(next) = consumers.get(id) {
            for dependent in next {
                let count = indegree
                    .get_mut(dependent)
                    .expect("validated task reference");
                *count -= 1;
                if *count == 0 {
                    ready.insert(dependent);
                }
            }
        }
    }
    if order.len() != tasks.len() {
        return Err(invalid("task dependencies contain a cycle"));
    }
    Ok(order)
}
