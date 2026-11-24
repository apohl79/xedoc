---
name: implement-with-evidence
description: Prepare and execute repository changes with evidence, minimal scope, end-to-end acceptance verification, safe worktrees, and checkpoint review. Use automatically before editing source, tests, configuration, prompts, skills, or public contracts, including config-only changes.
---

# Implement With Evidence

Use repository evidence to establish the edit boundary before changing files.

## Establish the gap

1. Read applicable repository instructions, manifests, implementation, tests,
   configuration, and public documentation.
2. Search related implementations, types, APIs, callers, tests, and
   documentation.
3. Trace the relevant flow end to end and identify existing behavior.
4. Stop without a production edit when no gap exists. Add only a regression
   test when coverage is the sole gap.

Before the first edit, report:

- `Existing behavior:` what the repository already does, with file references.
- `Gap:` what is missing or incorrect, with file references.
- `Plan:` the smallest complete change.
- `Scope:` the files expected to change.
- `Acceptance:` observable changed behaviors and the narrow end-to-end
  scenarios that will verify them.

Ask one concise question only when ambiguity or risk makes an assumption
unsafe; otherwise report the direction and continue.

## Define acceptance before implementation

- Translate the request into observable outcomes, not implementation details.
  For a bug, reproduce the reported failure through the relevant interface
  before editing when practical.
- Identify the actual entry point, consuming application, output or persisted
  state, and current artifact/environment needed for each scenario. Verify
  configuration through its consumer, not just its parser. Verify prompts and
  skills through fresh-agent tasks, not just their syntax.
- Cover every changed behavior with focused functional/user acceptance
  scenarios. Include relevant failure paths, persistence, or reverse
  transitions when they are affected; do not expand into unrelated coverage or
  broad suites.
- Establish how to execute the scenarios using available tools and safe
  isolated environments. A missing dependency, inaccessible environment, or
  unavailable build blocks acceptance; it does not make behavior untestable.
- Keep test actions within the task's authority. Ask before destructive actions,
  production changes, restarts, new integrations, or other additional
  authority. Do not substitute a mock for the boundary whose real behavior is
  being changed or claimed as verified.

## Select the workspace

- Inspect the branch and working tree before editing. Preserve unrelated
  changes.
- Use a worktree for implementation unless repository instructions or the user
  authorize the current checkout.
- Create worktrees under `../.my/worktrees/[repo]-[short-title]-[JIRA-if-exists]`.
- Allow specs and plans to use the current checkout when repository
  instructions permit it.

## Load relevant guidance

Before code or test edits, detect the languages and load matching production,
test, and framework skills. Repository instructions prevail.

## Choose the minimal implementation

After ruling out no change, use the first option that completely closes the
stated gap:

1. Delete or simplify code.
2. Reuse repository code or patterns.
3. Use the standard library.
4. Use a native platform feature.
5. Use an installed dependency.
6. Write minimum new code or add a dependency only when evidence requires it.

Fix root causes at the narrowest shared boundary; search direct callers before
changing shared functions. Prefer the smallest complete diff and fewest moving
parts. Avoid speculative abstractions, APIs, configuration, dependencies, and
refactors.

## Decompose and delegate

Treat a task as simple only when one obvious, low-risk edit has no independent
workstream.

1. Split the work into the minimum bounded, independently verifiable subtasks.
2. Give each worker a self-contained task with acceptance criteria, exclusive
   scope, relevant instructions, focused checks, and no authority beyond that
   scope.
3. Parallelize only disjoint work. Keep architecture, shared files,
   integration, and final verification with the primary agent.
4. After each complete behavior slice and after final integration, run a
   focused implementation review. Workers cannot review their own work.

Treat security, authorization, privacy, data-loss, migration, concurrency, and
public-contract changes as critical.

## Implement and verify

- Remove edits not required by the gap, acceptance criteria, or repository
  rules before validation.
- Follow repository formatting, lint, test, generation, and documentation
  rules.
- Proactively execute acceptance scenarios against the current changed
  artifact and configuration. Build or reload the isolated application as
  needed so an old binary or process cannot supply misleading evidence.
- Exercise the actual interface and inspect the outcome: browser action through
  the resulting download, TUI key binding through resulting state, CLI
  invocation through its output or side effect, or configuration through the
  consuming application.
- Treat compilation, linting, parsing, source inspection, unit tests, and
  reviewer approval as supporting checks, not substitutes for end-to-end
  acceptance. Prefer existing acceptance tools or short reproducible
  scenarios.
- Fix failures within scope and rerun affected scenarios after relevant code,
  configuration, formatting, lint-fix, or review edits. Retain unrelated valid
  evidence.
- Record each scenario's artifact/environment, action, expected result,
  observed result, and evidence. Distinguish passed, failed, blocked, and
  explicitly user-waived scenarios. Do not claim human UAT sign-off from
  agent-run acceptance alone.
- Map every changed behavior to passing acceptance evidence or an explicit
  user waiver. Explain any genuinely non-executable change.
- If required execution remains blocked after safe available routes, report
  **implemented but acceptance-unverified**, the concrete failure, and the
  remaining scenario or required access.
- Clean up the worktree when changes are safely retained; ask if follow-up
  timing is unclear.
- Report only behavior changes, material files, validation, and unresolved
  risk.
