---
name: esi-local-development
description: Execute software-development work with normal Goose tools while the local ESI controller enforces workspace plan approval, worktree binding, deterministic validation, repair budgets, resume state, and human gates.
---

Use this skill for writable software-development tasks managed by ESI-Studio.

## Workspace plan gate (mandatory — ADR-0010)

Before any coding, file edits, package installation, or shell implementation can happen, the workspace must have an **approved workspace plan**. This is enforced by the development controller in code — it is not optional.

### When starting work in a workspace:

1. **Check** with `workspaceplan__status`. The durable source remains
   `<workspace>/.esi/workspace-plan.json`.
2. **If no plan exists**: You are in **Discovery** mode. Help the user gather requirements:
   - Ask what they want to build and why
   - Identify acceptance criteria for each requirement
   - Prioritize requirements using MoSCoW (Must/Should/Could/Won't)
   - For greenfield/innovative work, run Innovation discovery: research the problem space, evaluate candidate approaches, and document the selected approach
3. **If the plan exists but is not approved**: Continue from the current plan status:
   - **Discovery** → continue gathering requirements
   - **Planning** → help design the implementation plan, architecture, and tasks
   - **Revising** → the plan changed after approval; review changes and seek re-approval
4. **If the plan is approved**: Load the plan context and proceed to the development workflow below.

### Plan approval flow:

- After requirements and implementation plan are complete, present them to the user for approval.
- Persist the complete draft with `workspaceplan__save_draft`; do not hand-author
  approval hashes or approval events.
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

- Use normal Goose reasoning, file, shell, search, and review tools for engineering work.
- Treat the local ESI development controller as the only authority for stages, validation evidence, failure routing, repair budgets, persisted state, and approvals.
- Never invent, skip, or rewrite a controller transition or event.
- Never approve worktree readiness, plan approval, extra repairs, completion, or abandonment yourself.
- Never claim completion while a required validator is failed, missing, or stale.
- Never call ForgeLoop, a private endpoint, or another remote orchestrator. This workflow is local and provider-neutral.
- **Never attempt to write code, edit files, install packages, or run implementation commands while the workspace plan is not approved.** The controller will reject these attempts.

## Workflow

1. **Workspace plan** (mandatory first step): Follow the workspace plan gate flow above.
2. Turn the user's objective and observable acceptance criteria into the `brief`.
3. Produce a bounded implementation plan and repository-native validation commands in this order: scope, syntax, static policy, lint/type/build, targeted tests, broader tests.
4. Wait for the controller's exact human-approved `worktree_ready` binding. Perform writable work only in that ESI-managed worktree.
5. During `implement` or `repair`, use normal Goose tools and keep changes limited to the approved brief and plan.
6. Ask the controller to run `deterministic_validate`; do not substitute your own success claim for its evidence.
7. At `diagnose`, use the controller's normalized failure fingerprint and category. Repair only when the controller routes to `repair`.
8. At `human_gate`, stop modifying files and present the pending fingerprint, exhausted budget, or abandonment request to the user.
9. At `review`, inspect the validated snapshot for correctness, regressions, security, and missing tests. A rejection returns to deterministic diagnosis and repair.
10. At `completion_gate`, stop modifying files. Completion requires explicit human approval for the exact validated snapshot.

On resume, load the controller-owned state and events, confirm the worktree binding, and continue only from the persisted current stage.