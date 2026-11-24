---
name: research-and-investigations
description: Research and investigate cross-service operational, architecture, and performance questions. Use when evidence from repositories, logs, dashboards, deployments, or configuration must be gathered and synthesized to explain a problem, answer a decision, or recommend a change.
---

# Research and Investigations

Use this workflow to produce an evidence-led answer without creating an
open-ended agent swarm. The primary agent owns the conclusion, safety checks,
and any writes.

## 1. Frame the question

State:

- The decision or diagnosis required.
- The exact time window, environment, and components in scope.
- The available evidence sources, plus any source that is unavailable.
- The distinction between measured facts, derived conclusions, and hypotheses.

Never install plugins, add MCP servers, create credentials, or substitute a
different observability route when the requested one is unavailable. Report
that gap plainly.

## 2. Decompose by evidence, not by components

Use a small preprocessing task for a narrow, independent read-only job, such
as:

- Locate logs, artefacts, jobs, deployments, and revisions in a time window.
- Map a dashboard metric or alert to the source code that emits it.
- Extract event semantics, guardrails, or configuration limits.
- Inventory configuration across a fixed list of services.

Use one synthesis task only when the remaining question needs deeper synthesis:
competing architecture designs, distributed-systems correctness, or a ranked
causal analysis. Give it raw evidence and explicit open questions; do not ask it
to repeat preprocessing.

Do simple local inspection directly. Do not give two agents overlapping
searches.

## 3. Delegate through Xedoc routing

Treat work as simple only when it can be completed through one obvious,
low-risk local inspection.

1. Split the work into the minimum bounded, independently verifiable evidence
   tasks.
2. Give each worker exclusive scope, acceptance criteria, a finite stopping
   condition, and minimal context. Disable further delegation.

For a task needing only one role, dispatch only that role.

## 4. Bound every delegated task

Every subagent prompt must include:

- A concrete deliverable.
- Exact sources, repository paths, and/or time window.
- Read-only or write authority.
- Explicitly prohibited actions.
- Required evidence format: file paths, line references, command output, or
  source URLs.
- A finite stopping condition.

Example:

```text
Bounded, read-only task. Inspect only <paths> and commits since <sha>.
Determine what emits <metric> and what each outcome means. Do not edit,
query external observability systems, or recommend implementation changes.
Return: (1) source locations, (2) semantics, (3) what it proves and does
not prove, and (4) a maximum of three evidence-backed hypotheses.
```

Set an appropriate time budget. If a worker becomes redundant or exceeds its
useful bound, stop it and synthesize the evidence already collected.

## 5. Synthesize causally

Correlate timestamps with deployment revisions, restarts, scaling events, and
configuration changes. Classify each observed signal separately:

| Class | Interpretation |
| --- | --- |
| Direct service failure | An error, saturation, or unavailable dependency with a causal path to the symptom. |
| Integration/topology mismatch | Publisher, consumer, binding, or lifecycle state disagrees with expected topology. |
| Expected race or guardrail | A deliberately bounded failure mode or lifecycle transition that needs rate and context before it is called a defect. |
| Insufficient evidence | A dashboard count without necessary raw dimensions, historical series, or trace/log correlation. |

Do not infer historical CPU, latency, or capacity from a current Kubernetes
snapshot. Label extrapolations as estimates and state assumptions.

## 6. Close responsibly

Lead with the answer. Include:

- What is confirmed, with evidence.
- What remains unknown and why.
- A ranked, minimal next diagnostic or implementation action.
- Whether a proposed optimization changes correctness or only cost/latency.

Before writing project context, search existing records. Store only verified
durable architecture decisions or user-confirmed invariants, never transient
PR state, current measurements, or debugging hypotheses. Update an existing
record rather than creating overlap.
