---
name: esi-local-development
description: Execute software-development work with normal Goose tools while the local ESI controller enforces worktree binding, deterministic validation, repair budgets, resume state, and human gates.
---

Use this skill for writable software-development tasks managed by ESI-Studio.

## Authority boundary

- Use normal Goose reasoning, file, shell, search, and review tools for engineering work.
- Treat the local ESI development controller as the only authority for stages, validation evidence, failure routing, repair budgets, persisted state, and approvals.
- Never invent, skip, or rewrite a controller transition or event.
- Never approve worktree readiness, extra repairs, completion, or abandonment yourself.
- Never claim completion while a required validator is failed, missing, or stale.
- Never call ForgeLoop, a private endpoint, or another remote orchestrator. This workflow is local and provider-neutral.

## Workflow

1. Turn the user's objective and observable acceptance criteria into the `brief`.
2. Produce a bounded implementation plan and repository-native validation commands in this order: scope, syntax, static policy, lint/type/build, targeted tests, broader tests.
3. Wait for the controller's exact human-approved `worktree_ready` binding. Perform writable work only in that ESI-managed worktree.
4. During `implement` or `repair`, use normal Goose tools and keep changes limited to the approved brief and plan.
5. Ask the controller to run `deterministic_validate`; do not substitute your own success claim for its evidence.
6. At `diagnose`, use the controller's normalized failure fingerprint and category. Repair only when the controller routes to `repair`.
7. At `human_gate`, stop modifying files and present the pending fingerprint, exhausted budget, or abandonment request to the user.
8. At `review`, inspect the validated snapshot for correctness, regressions, security, and missing tests. A rejection returns to deterministic diagnosis and repair.
9. At `completion_gate`, stop modifying files. Completion requires explicit human approval for the exact validated snapshot.

On resume, load the controller-owned state and events, confirm the worktree binding, and continue only from the persisted current stage.