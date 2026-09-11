---
name: scheduling-orchestration
description: Schedule bounded dependent subagent work for a parent task without duplicating routing policy. Use when a task needs a dependency-aware plan, parallel read-only investigation, isolated write work, bounded concurrency or spend, and a final integration step.
---

# Scheduling Orchestration

Own the requested outcome and use subagents only for bounded work that advances it. Do not
schedule work merely to fill capacity.

## Define the boundary

Before delegating, state the evidence boundary, final artifact, acceptance criteria, and explicit
stopping condition. Inspect enough existing code, documents, and task context to distinguish
confirmed facts from assumptions.

## Build the task graph

Decompose only independent, bounded work. For every task, record:

- Deliverable and acceptance criteria.
- Dependencies and required input artifacts.
- Read or write scope, permitted paths or systems, and parallel-safety.
- Expected cost or time budget and a finite stopping condition.

Do direct local work when it is simpler than delegation. Do not assign overlapping searches,
writes, or conclusions to multiple agents.

## Schedule safely

Run only dependency-ready tasks. Keep concurrent work within the parent task's available
concurrency and spend limits; reserve capacity for integration and required review. Re-evaluate the
graph when a task returns evidence that invalidates a dependency or makes another task redundant.
Do not create worktrees, branches, external resources, or additional capacity unless the parent
task authorizes them.

Maintain an auditable schedule before each dispatch. For every task, record its identifier,
dependencies, scope, parallel-safety, resource budget, assigned worktree or branch when applicable,
input artifacts, output contract, status, and any router decision received. Record the dispatch and
completion evidence, plus every skipped, deferred, or blocked task and its reason.

For write tasks, require an isolation record before scheduling:

- task and A/B group identifiers;
- base revision and isolated branch or worktree location;
- exclusive write paths or other effect boundary; and
- merge owner and integration order.

If isolation cannot be proven, schedule one ordinary write task rather than an A/B pair.

## Respect routing decisions

Treat a model-router decision as dispatch input only: record it with the schedule and apply it only
through the normal delegation mechanism. Never select, rank, override, or duplicate routes,
providers, models, or reasoning effort. When routing is absent or off, schedule the same bounded
prompt through the normal fallback path.

## Delegate with complete prompts

Each prompt must state the parent objective, assigned deliverable, supplied inputs, dependencies,
read/write authority, prohibited actions, required evidence format, acceptance criteria, and
stopping condition. Include the task's scope and isolation record when applicable.

Do not add ordinary model, provider, or reasoning-effort overrides to prompts. Send the task prompt
unchanged to the normal delegation mechanism so the configured router may make its own decision.

## Integrate and close

Validate returned artifacts against their contracts, resolve conflicts at the parent, and produce the
requested final artifact. Schedule review, PR finalization, or deployment only when the parent task
explicitly calls for it. Stop when the requested outcome and its acceptance criteria are complete;
report unresolved evidence or blocked dependencies instead of extending the workflow.
