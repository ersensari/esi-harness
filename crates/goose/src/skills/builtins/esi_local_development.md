---
name: esi-local-development
description: Execute software-development work with normal ESI-Studio tools while the local ESI controller enforces workspace plan approval, worktree binding, deterministic validation, repair budgets, resume state, and human gates.
---

Use this skill for writable software-development tasks managed by ESI-Studio.

## Choose the execution policy first (ADR-0020)

Trusted extensions may execute their native tools without workspace-plan containment,
subject to the operator's ordinary tool permissions. Do not invent a blanket plan
approval requirement for that trusted workflow. Trust does not manufacture human
approval, validation evidence, or controller completion.

When using the structured ESI development controller, its approved workspace plan,
worktree binding and evidence gates remain mandatory. Follow the workflow below
for controller-managed work; keep ordinary trusted-tool work clearly distinct.

### When starting work in a workspace:

1. **Check** with `workspaceplan__status`. The durable source remains
   `<workspace>/.esi/workspace-plan.json`.
2. **If no plan exists**: You are in **Discovery** mode. Help the user gather requirements:
   - Ask what they want to build and why
   - Identify acceptance criteria for each requirement
   - Prioritize requirements using MoSCoW (Must/Should/Could/Won't)
   - For a small change, call `workspaceplan__create_template` with `template: small_change`;
     this creates three bounded tasks, not a full-project discovery interview.
   - For a new project, use `template: greenfield` (five tasks), and investigate
     alternatives only where the scope actually needs an architecture decision.
   - Template creation never overwrites an authored plan or existing requirements.
3. **If the plan exists but is not approved**: Continue from the current plan status:
   - **Discovery** → continue gathering requirements
   - **Planning** → help design the implementation plan, architecture, and tasks
   - **Revising** → the plan changed after approval; review changes and seek re-approval
4. **If the plan is approved**: Load the plan context and proceed to the development workflow below.

### Plan approval flow:

- After requirements and implementation plan are complete, present them to the user for approval.
- Persist the complete draft with `workspaceplan__save_draft`; do not hand-author
  approval hashes or approval events.
- Refine `task_contracts` keyed by task ID: `depends_on`, `affected_components`,
  `acceptance_criteria` (`id`, `description`, optional `requirement_id`) and
  `validation_expectations` (`id`, `description`, `criterion_ids`). Expectations
  describe future checks; they are not successful validation evidence. Status
  returns contracts and deterministic `task_execution_order` for reuse across chats.
- The user must explicitly approve the plan. You cannot approve it yourself.
- Only after the user accepts the displayed plan, call `workspaceplan__approve`.
  Desktop always presents a confirmation for this tool, even in automatic mode.
- Once the user confirms that action, the plan status changes to `approved` and
  implementation is unblocked.
- If the plan is modified after approval (requirements, architecture, tasks), approval is automatically invalidated and the user must re-approve.

### Plan is durable across chats:

The workspace plan persists at `.esi/workspace-plan.json`. When a new chat opens in the same workspace:
- Load the existing plan
- Present its current status to the user
- Continue from where the last session left off
- Never restart the requirements interview from scratch

## Authority boundary

- Use normal ESI-Studio reasoning, file, shell, search, and review tools for engineering work.
- Treat the local ESI development controller as the only authority for stages, validation evidence, failure routing, repair budgets, persisted state, and approvals.
- Never invent, skip, or rewrite a controller transition or event.
- Never approve worktree readiness, plan approval, extra repairs, completion, or abandonment yourself.
- Never claim completion while a required validator is failed, missing, or stale.
- Never call ForgeLoop, a private endpoint, or another remote orchestrator. This workflow is local and provider-neutral.
- For controller-managed work, do not run implementation outside its approved
  plan/worktree. For ordinary trusted extension work, honor the operator's Trust
  and tool permissions without adding this controller prerequisite.

## Workflow

1. **Workspace plan** (mandatory first step): Follow the workspace plan gate flow above.
2. Turn the user's objective and observable acceptance criteria into the `brief`.
3. Produce a bounded implementation plan and repository-native validation commands in this order: scope, syntax, static policy, lint/type/build, targeted tests, broader tests.
4. Wait for the controller's exact human-approved `worktree_ready` binding. Perform writable work only in that ESI-managed worktree.
5. During `implement` or `repair`, use normal ESI-Studio tools and keep changes limited to the approved brief and plan.
6. Ask the controller to run `deterministic_validate`; do not substitute your own success claim for its evidence.
7. At `diagnose`, use the controller's normalized failure fingerprint and category. Repair only when the controller routes to `repair`.
8. At `human_gate`, stop modifying files and present the pending fingerprint, exhausted budget, or abandonment request to the user.
9. At `review`, inspect the validated snapshot for correctness, regressions, security, and missing tests. A rejection returns to deterministic diagnosis and repair.
10. At `completion_gate`, stop modifying files. Completion requires explicit human approval for the exact validated snapshot.

On resume, load the controller-owned state and events, confirm the worktree binding, and continue only from the persisted current stage.

## Automatic workspace visualizer

- At the start of a workspace-bound development chat, call
  `esi-development-visualizer__show_development_loop` with `workspace_path` set
  to the canonical working directory. Do not ask the user to locate a state
  file.
- Refresh the visualizer after plan approval or revision, deterministic
  validation, repair routing, and completion/abandonment gates.
- The visualizer discovers controller state automatically. When no controller
  state exists it labels its result `live_workspace`; treat that as a
  read-only repository snapshot, never as invented controller evidence.
- File tree, file content, Git diff, and canvas interactions remain read-only.
  They do not satisfy validation or approval requirements.
