# Automatic Model Router — Design

Status: Proposed
Owner: fork maintainers
Scope: model-routing runtime policy, TUI, calibration, reporting, and orchestration

## 1. Summary

Xedoc should route each eligible task to a provider, model, and reasoning effort
appropriate for that task. A task is either:

- the new user prompt that starts a root turn; or
- the complete prompt passed to a newly spawned subagent.

The router is an optional runtime policy. It is separate from the orchestration skill:
the skill decides how work should be decomposed and scheduled, while the runtime
classifies each resulting task prompt and chooses its execution route.

Classification runs locally and does not require Ollama or a hosted embedding API. The
selected embedding backbone is `snowflake-arctic-embed-xs`, run through FastEmbed with
a pinned ONNX artifact. An embedding model produces a vector, not a class, so Xedoc
must add a deterministic classifier: normalized prompt embedding → versioned
classifier → confidence/margin gates → mapped route or fallback.

The feature supports direct routing and shadow routing from the start. Every decision
is visible in the TUI and persisted for reporting. A one-turn A/B modifier duplicates
eligible subagent tasks: branch A uses the routed model and branch B uses the parent
orchestrator's model. Actual usage, calculated cost, normalized baseline cost, A/B
overhead, and savings are reported per task, turn, session, class, route, and day in
an app-server-hosted web UI opened through `/model-router report`.

## 2. Decisions

- Routing is a runtime policy; decomposition and scheduling are a skill discipline.
- Root-turn routing is optional, not a later extension.
- All five modes ship together: `off`, `shadow-subagents`, `shadow-full`,
  `subagents`, and `full`. Shadow mode is supported but is not a prerequisite for
  direct routing.
- Root prompts and subagent prompts use the same classifier and policy.
- The runtime classifier is local and deterministic. No LLM call is required to route
  a task.
- Arctic Embed XS is the initial embedding backbone, selected by the historical
  held-out benchmark in section 11.
- Provider identity is explicit. A route is `(provider_id, model_slug,
  reasoning_effort)`, never just a model-looking string.
- The tunable class/prototype/route mapping lives in
  `~/.xedoc/model-router.toml`. General controls remain in
  `~/.xedoc/config.toml`.
- The default reporting baseline is configurable and initially points at
  `openai / gpt-5.6-sol / xhigh`.
- Ordinary baseline savings are a labelled counterfactual estimate. A/B branch costs
  are observed and therefore can be compared directly.
- Reporting is a browser UI owned and hosted by the app-server. The TUI opens it through
  the `/model-router report` slash command.
- Route decisions are UI/protocol events, not model-visible context items.

## 3. Goals and non-goals

Goals

- Reduce model spend without giving up quality on tasks that need a strong model.
- Route both root turns and subagents, under explicit user control.
- Derive the initial task taxonomy from Xedoc's historical sessions rather than
  inventing an exhaustive taxonomy upfront.
- Make every route and fallback visible, attributable, and reproducible.
- Let users edit and regenerate the class-to-route mapping without rebuilding Xedoc.
- Support one-turn paired comparisons for tuning.
- Account for reported tokens, provider-aware cost, counterfactual baseline cost,
  savings, and experiment overhead.
- Remain local, fast, bounded, cross-platform, and independent of Ollama.
- Preserve explicit user model choices and existing safety routing.

Non-goals

- Predict the exact best answer quality before inference.
- Treat historical model choices as ground-truth labels for quality.
- Rewrite existing conversation history when a route changes.
- Put raw historical prompts, embeddings, or unbounded decision history into model
  context.
- Automatically edit the route mapping from weak signals without a reviewable
  calibration result.
- Replace provider/model availability checks or safety rerouting.
- Make A/B mode a general concurrent implementation system; the orchestration skill
  remains responsible for write isolation and integration.

## 4. Existing seams

| Concern | Existing seam | Router implication |
|---|---|---|
| Root model and effort | `TurnContext` contains model/provider/reasoning; it is built per turn | Route before the final turn context and inference request are created |
| Subagent model and effort | `multi_agents_common` resolves explicit overrides and configured defaults before spawn | Insert automatic policy resolution at this boundary |
| Model/provider changes | session config and `ModelClient` already support route changes | Reuse the existing provider-aware switching path |
| Model catalog | provider-aware model metadata and supported efforts | Validate every configured route against the live catalog |
| Safety reroute | `ModelRerouteEvent` currently represents high-risk cyber rerouting | Add a distinct router-decision event; do not overload the safety event |
| Token/cost accounting | `SessionCostTracker`, `TokenUsage`, and `ModelTokenPrices` | Attach decision/branch IDs and persist route-attributed rows |
| Slash commands | TUI completion plus chat-widget dispatch | Add `/model-router` without growing central modules unnecessarily |
| Browser launch | TUI `OpenUrlInBrowser` event | Reuse it after the app-server returns a report URL |
| App-server HTTP | WebSocket transport already uses Axum for health and upgrade routes; embedded/stdio modes have no HTTP listener | Use a separate lazy report listener so the UI also works with the normal embedded TUI |
| Durable state | `xedoc-state` SQLite and migrations | Store decisions, observed usage, and bounded daily rollups |

A model change can lose provider prompt-cache reuse and force a full history replay.
Reporting must include cached input tokens and route switches; a cheap output model is not necessarily a cheap turn.

## 5. Historical calibration evidence

A read-only scan of `~/.xedoc/sessions` on 2026-09-11 found:

- 4,463 rollout files spanning 2025-12-21 through 2026-09-10;
- 2,120 subagent sessions;
- 18,277 recorded turn contexts;
- substantial use of multiple model families and reasoning efforts;
- 10,168 user-message events, 12,394 completed-task events, and 794 aborted-turn
  events.

The largest observed turn-context groups were `gpt-5.6-sol` (4,919),
`gpt-5.6-luna` (4,563), and `gpt-5.6-terra` (3,484). The largest reasoning-effort
groups were `xhigh` (9,044), `medium` (5,737), and `high` (1,722).

This is enough data to discover recurring task shapes, estimate class frequency and
cost, and construct a calibration set. It is not enough to infer that a historical
route was optimal: model selection was not randomized, completion is only a weak
quality signal, and aborted turns have multiple causes. Paired A/B runs provide the
stronger tuning evidence over time.

Historical analysis must remain local. Calibration reads only the initial root prompt
or initial subagent prompt plus bounded structural metadata. It must not duplicate raw
prompts into the policy file or metrics database.

## 6. Architecture

```text
new root prompt ───────────────┐
                              │
new subagent prompt ──────────┤
                              ▼
                    ┌─────────────────────┐
                    │ xedoc-model-router  │
                    │ normalize + bound   │
                    │ local embedding     │
                    │ classifier scoring  │
                    │ policy + fallback   │
                    └─────────┬───────────┘
                              │ RouteDecision
                  ┌───────────┴───────────┐
                  ▼                       ▼
          shadow: keep route      direct: apply route
                  │                       │
                  └───────────┬───────────┘
                              ▼
                root TurnContext or subagent spawn
                              │
                 usage/cost + completion outcome
                              ▼
             state DB ──► TUI message/report/rollup
```

### 6.1 New crate

Put the learned-routing logic in a new `xedoc-model-router` crate rather than adding a
new concept to `xedoc-core`.

Small public API:

```rust
pub struct TaskEnvelope<'a> {
    pub scope: RouteScope,
    pub prompt: &'a str,
    pub parent_route: Option<&'a ModelRoute>,
}

pub struct ModelRoute {
    pub provider_id: String,
    pub model_slug: String,
    pub reasoning_effort: ReasoningEffort,
}

pub struct RouteDecision {
    pub class_id: String,
    pub score: f32,
    pub margin: f32,
    pub proposed_route: ModelRoute,
    pub effective_route: ModelRoute,
    pub disposition: RouteDisposition,
    pub reason: DecisionReason,
    pub policy_revision: String,
}

pub trait TaskEmbedder {
    fn embed(&self, prompt: &str) -> Result<Vec<f32>, EmbedError>;
}

pub fn decide(
    task: TaskEnvelope<'_>,
    policy: &RoutingPolicy,
    catalog: &ModelCatalog,
    embedder: &dyn TaskEmbedder,
) -> RouteDecision;
```

Keep embedding, classifier scoring, policy parsing, catalog validation, and reporting
types in separate private modules. Loading the model is lazy and process-wide.

### 6.2 Task input

The classifier receives only the newly submitted task prompt, not the entire
conversation. Before embedding:

1. remove transport-only wrappers and the A/B branch label;
2. preserve code fences, paths, tool names, and task verbs because they carry intent;
3. normalize insignificant whitespace;
4. truncate at a fixed token/byte boundary, keeping the start and end;
5. record only the input hash, byte count, and truncation flag.

The input has a hard cap and is never added to model-visible context by the router.

### 6.3 Embedding is not the classifier

The embedding model maps a prompt to a vector. Classification needs additional,
deterministic logic:

1. L2-normalize the vector.
2. Apply the versioned classifier selected by calibration.
3. Produce one deterministic score per class.
4. Select the highest-scoring class only when both its absolute score and its margin
   over the runner-up meet configured thresholds.
5. Otherwise abstain and use the configured fallback route.

This avoids a second model at runtime. It also makes decisions reproducible from the
embedding artifact, policy revision, prompt hash, and thresholds.

Prototype k-nearest-neighbour remains the explainable diagnostic baseline. Because its
measured quality is inadequate, the first routed version must compare it with a
deterministic linear head over frozen embeddings and select the classifier through
predeclared held-out quality and abstention gates.

### 6.4 Policy precedence

Route selection follows this order:

1. non-negotiable safety/provider restrictions;
2. an explicit user/provider/model/effort override;
3. an A/B branch assignment;
4. the automatic router when the current mode includes the task scope;
5. configured subagent defaults or inherited parent route.

An invalid or unavailable configured route never silently changes provider identity.
It produces a visible fallback decision. Existing high-risk rerouting remains
independent and can override the effective route after the model router runs.

## 7. Router modes

`/model-router` exposes exactly these persistent modes:

| Mode | Root turns | Subagents | Applies route |
|---|---:|---:|---:|
| `off` | no classification | no classification | no |
| `shadow-subagents` | no | classify | no |
| `shadow-full` | classify | classify | no |
| `subagents` | no | classify | subagents only |
| `full` | classify | classify | yes |

Shadow decisions use the same classifier, validation, fallback logic, events, and
reporting as direct decisions. Their disposition is `shadow`, and usage remains
attributed to the effective non-routed model.

Root routing happens after the new prompt is accepted but before the root
`TurnContext` and request snapshot are finalized. Subagent routing happens after the
orchestration skill or model has produced the complete agent prompt and immediately
before spawn configuration is finalized.

Low confidence, embedding failure, missing artifact, stale policy, unsupported effort,
or unavailable provider all cause a configured fallback. The default fallback is the
current orchestrator route; the TUI states the reason.

## 8. One-turn A/B mode

`/model-router ab next` arms A/B mode for the next submitted root turn.
`/model-router ab off` cancels the pending modifier. The modifier is consumed once
that turn begins and is recorded in its rollout; it is not a persistent global mode.

For every eligible subagent prompt emitted during that turn:

- branch A receives the byte-identical task prompt and uses the automatic routed
  provider/model/effort;
- branch B receives the same task prompt and uses the parent orchestrator's route
  captured at turn start;
- both branches are leaf agents by default, preventing recursive doubling;
- branch identity is transport metadata and is excluded from classification;
- both results return to the orchestrator, tagged with their branch and decision ID.

A/B is a modifier, so it can collect calibration data even when normal routing is
`off` or shadow-only. Tasks with an explicit model override are excluded and reported
as ineligible.

For read-only analysis, branches can run concurrently. For implementation or other
write-capable work, the orchestration skill must assign separate worktrees or otherwise
isolate writes; the root agent owns comparison and integration. If isolation cannot be
established, A/B fails closed for that task instead of letting both agents edit the
same tree.

A/B reporting separates:

- routed branch cost;
- orchestrator branch cost;
- paired delta;
- experiment overhead from running the second branch;
- the orchestrator's quality preference, tie, or unusable result.

The configured reporting baseline may differ from the live orchestrator route; the UI must show both.

## 9. Orchestration skill

Create a new orchestration skill informed by `orchestrating-bounded-research` and
`implement-with-evidence`, but focused on task composition, dependency-aware
scheduling, and integration.

Its responsibilities are:

1. establish the evidence boundary and the desired final artifact;
2. decompose the work into the smallest independently useful tasks;
3. mark dependencies, read/write scope, and whether parallel execution is safe;
4. issue complete, bounded subagent prompts with an explicit output contract;
5. schedule ready tasks within a concurrency and spend budget;
6. collect evidence, integrate implementation, schedule review/finalization/deployment
   tasks when required, and stop when the outcome is complete.

When routing is active, the skill does not encode model names or reasoning efforts into
ordinary agent prompts. The runtime classifies each finished prompt. When routing is
off, explicit agent overrides and configured defaults continue to work.

The classifier should discover the durable taxonomy from history, but likely seed task
families include documentation analysis, code analysis, research, implementation,
review, PR finalization, and deployment/operations. These are starting hypotheses, not
hard-coded runtime enums; calibration may merge or split them.

## 10. Configuration

### 10.1 Runtime controls

`~/.xedoc/config.toml` owns stable user controls:

```toml
[model_router]
mode = "full"
policy_path = "~/.xedoc/model-router.toml"
fallback = "orchestrator"
max_prompt_bytes = 16384

[model_router.baseline]
provider = "openai"
model = "gpt-5.6-sol"
reasoning_effort = "xhigh"
```

`Feature::ModelRouter` gates the feature's availability. `mode = "off"` is the normal
operational switch. `/model-router mode <mode>` updates the config atomically and takes
effect on the next eligible task; it never rewrites an in-flight turn.

### 10.2 Tunable policy

`~/.xedoc/model-router.toml` owns generated and hand-tunable calibration data:

```toml
schema_version = 1
policy_revision = "2026-09-11.1"

[embedding]
runtime = "fastembed"
model = "Snowflake/snowflake-arctic-embed-xs"
revision = "<pinned full commit SHA>"
artifact_sha256 = "<sha256>"
dimensions = 384

[classifier]
revision = "classifier-2026-09-11.1"
parameters_sha256 = "<sha256>"
minimum_score = 0.70
minimum_margin = 0.08

[[classes]]
id = "code-analysis"
provider = "deepseek"
model = "deepseek-flash"
reasoning_effort = "medium"

[[classes]]
id = "implementation"
provider = "openai"
model = "gpt-5.6-terra"
reasoning_effort = "high"
```

The threshold numbers are illustrative; calibration supplies them. The real file
stores bounded classifier parameters and opaque local source IDs, not raw prompt text.
Use an atomic replace and retain the previous valid revision for rollback.
Startup validates the schema, embedding revision/dimensions, unique class IDs,
thresholds, provider/model existence, and supported reasoning efforts. An artifact or
dimension change invalidates all existing prototypes until recalibration succeeds.

## 11. Embedding runtime and model choice

### 11.1 Recommendation

Use [`snowflake-arctic-embed-xs`](https://huggingface.co/Snowflake/snowflake-arctic-embed-xs)
through [`fastembed-rs`](https://github.com/Anush008/fastembed-rs). It won the
predeclared selection rule on the held-out historical Xedoc corpus, including a
cue-masked robustness view. Its roughly 1.9 ms single-prompt median on the benchmark
host is small relative to a model turn.

Package a pinned model artifact with Xedoc release assets under
`~/.xedoc/model-router/models/<revision>/`. Verify its hash and disable remote
downloads in release builds.

### 11.2 Options

| Option | Strength | Cost/risk | Recommendation |
|---|---|---|---|
| Model2Vec + Potion 8M | Smallest runtime, no ONNX, extremely fast, local-only loading | Static embeddings trailed Arctic by 12.3 macro-F1 points | Keep as an explicit low-memory option, not the default |
| [`bge-small-en-v1.5`](https://huggingface.co/BAAI/bge-small-en-v1.5) via FastEmbed | Strongest raw top-two recall in this run | Lower macro-F1 and slower than Arctic; ONNX packaging | Runner-up |
| Arctic Embed XS via FastEmbed | Best raw and cue-masked macro-F1; 1.9 ms median | ONNX/native packaging and roughly 190 MiB loaded RSS | Selected |
| `all-MiniLM-L6-v2` via FastEmbed | Mature, compact transformer baseline | Still pays ONNX/native packaging cost; generic baseline rather than task-specialized | Benchmark control |
| [`jina-embeddings-v2-base-code`](https://huggingface.co/jinaai/jina-embeddings-v2-base-code) | English plus 30 programming languages, 8,192-token support, Apache-2.0 | 161M parameters and code-retrieval orientation are excessive for short intent classification | Do not use as default |
| [`candle`](https://github.com/huggingface/candle) plus a transformer | Rust-native ML framework and control over model execution | More model/pooling/tokenizer code owned by Xedoc; much larger implementation surface | Revisit only if ONNX packaging blocks the stronger fallback |
| Hosted embeddings API | No local artifact packaging | Network latency, recurring cost, credentials, privacy, and offline failure | Exclude from v1 |

FastEmbed's `ort` dependency adds a native ONNX Runtime binary for each OS/CPU
architecture, loader/version coordination, release packaging/signing, and extra
binary/RSS cost. Hardware execution providers add more shared libraries. The
benchmark also saw a 3.8 GiB RSS delta while Arctic encoded one 2,336-prompt batch;
offline calibration must use bounded batches or disposable workers. Runtime routing
embeds one bounded task, so loaded RSS and single-prompt latency are the relevant
steady-state measures.

If historical prompts contain enough non-English tasks to affect coverage, add
`multilingual-e5-small` through the same FastEmbed benchmark. Do not switch merely
because a multilingual model exists; select it on held-out routing evidence.

### 11.3 Measured selection benchmark

The 2026-09-11 benchmark scanned 9,557 messages through 2026-09-09 and retained 1,168
deduplicated tasks with labels derived primarily from subsequent tool behavior. The
five classes were analysis, delivery, documentation, implementation, and review.
Each class used a chronological 60/20/20 train/validation/test split (699/234/235).
The same deterministic prototype classifier was tuned on validation embeddings for
every model. A second view masked obvious intent words to test cue robustness.

| Model | Raw macro-F1 | Masked macro-F1 | Mean | p50 ms | Batch prompts/s | Load ΔRSS |
|---|---:|---:|---:|---:|---:|---:|
| Arctic XS | 0.427 | 0.439 | **0.433** | 1.90 | 26.7 | 190 MiB |
| BGE small v1.5 | 0.406 | 0.423 | 0.415 | 5.73 | 9.8 | 167 MiB |
| MiniLM L6 | 0.395 | 0.378 | 0.387 | 7.09 | 129.8 | 190 MiB |
| Potion 8M | 0.326 | 0.293 | 0.310 | 0.04 | 12,060.1 | 68 MiB |

Before examining results, the rule selected the quality winner unless Potion was
within 0.015 macro-F1 on both views with no masked class-recall deficit over 0.05.
Arctic won. Measurements used Python 3.12 on Darwin arm64 with FastEmbed 0.8.0, ONNX
Runtime 1.30.0, and Model2Vec 0.9.0. Throughput is bulk; latency is single-prompt.

This selects the embedding backbone, not a production-ready classifier. A 0.433
macro-F1 ceiling is too weak for unrestricted direct routing. Before activation,
benchmark a deterministic linear head over the frozen Arctic vectors, refine labels,
and establish cost-weighted error plus abstention gates. Operations had only 33
high-confidence examples and remain outside the learned classes with an explicit
strong-model floor. Only 38 sampled tasks were subagent prompts, so the next corpus must
improve that coverage. Cross-platform determinism and packaging remain gates.

## 12. Calibration and tuning

### 12.1 Historical bootstrap

An offline `xedoc model-router calibrate` workflow should:

1. enumerate completed root and subagent sessions;
2. extract only initial task prompts and bounded metadata;
3. deduplicate exact and near-identical prompts;
4. create embeddings using the pinned candidate artifact;
5. cluster prompts across a range of class counts;
6. identify stable clusters, medoids, outliers, and ambiguous boundaries;
7. assign initial task-family names from structural metadata and a local review of
   bounded medoid samples;
8. create a stratified held-out set;
9. benchmark embedding candidates and confidence thresholds;
10. emit a reviewable policy diff and calibration report.

The calibration command never overwrites the active policy directly. An explicit
`activate` step atomically promotes a reviewed revision.

Initial route mappings should be conservative. Historical route frequency and cost can
inform candidate mappings, but cannot prove quality. Ambiguous classes and high-impact
work default to the orchestrator/baseline route until A/B evidence supports a cheaper
choice.

### 12.2 Continuous tuning

A/B outcomes add paired records keyed by class and prompt hash. The orchestrator records
which result it preferred and whether either branch was unusable. Tuning reports
aggregate:

- win/tie/loss and unusable rates by class and route;
- routed/orchestrator cost ratio;
- latency delta;
- confidence deciles versus observed preference;
- fallbacks and manual overrides;
- drift in class frequency and nearest-prototype distance.

Policy generation may recommend a new route, effort, threshold, class merge/split, or
new prototypes. Activation remains explicit and reversible.

## 13. Events and TUI

Add a dedicated `ModelRouterDecisionEvent`; do not reuse `ModelRerouteEvent`, whose
meaning is a safety reroute. The new event includes:

- decision ID, thread ID, turn ID, and a closed root/subagent lineage value;
- root/subagent scope and active router mode;
- prompt hash and bounded input size metadata;
- a closed classified/failed outcome carrying class ID, score, and margin when classified;
- policy/artifact revisions plus proposed, effective, fallback, orchestrator, and
  reporting-baseline routes;
- disposition (`applied`, `shadow`, `fallback`, `explicit`, `ab-a`, or `ab-b`);
- machine-readable reason and elapsed classifier time.

The TUI renders one compact, non-model-visible message per decision:

```text
Router · subagent · implementation 0.84 (+0.12)
terra/high → deepseek-flash/medium · applied
```

Shadow mode says `would route`; fallback says why; A/B messages show `A routed` and
`B orchestrator`. Provider names are shown when slugs would be ambiguous.

Command surface:

```text
/model-router
/model-router mode off
/model-router mode shadow-subagents
/model-router mode shadow-full
/model-router mode subagents
/model-router mode full
/model-router ab next
/model-router ab off
/model-router report [days]
/model-router decisions
```

With no arguments, show mode, feature/artifact health, active policy revision,
baseline, pending A/B state, session actual cost, baseline estimate, savings, A/B
overhead, and fallback rate.

`/model-router report [days]` asks the active app-server for a report URL and sends that
URL through the existing TUI browser-launch event. The default range is 30 days and
the server enforces a bounded maximum. `/model-router decisions` remains a compact TUI
view for recent routing diagnostics; it is not the primary reporting surface.

## 14. Accounting and reporting

### 14.1 Actual usage and cost

For each model invocation, persist provider-reported token buckets:

- non-cached input;
- cached input;
- output, including reasoning when the provider includes it;
- total.

Capture the provider/model price revision with the row. Calculated actual cost is:

```text
actual =
    non_cached_input × input_price
  + cached_input     × cached_input_price
  + output           × output_price
```

All prices are normalized per token before the calculation. If usage or any required
price is unavailable, cost is `unknown`, not zero. Token counts remain reportable.

Auxiliary model calls and both A/B branches count toward actual session spend. Local
embedding inference records latency and CPU time but has no provider-token cost.

### 14.2 Baseline and savings

For an ordinary routed invocation, Xedoc cannot know the exact tokens the baseline
model would have produced. The initial counterfactual therefore reprices the observed
token buckets using the configured baseline route:

```text
normalized_baseline =
    observed_non_cached_input × baseline_input_price
  + observed_cached_input     × baseline_cached_input_price
  + observed_output           × baseline_output_price

estimated_savings = normalized_baseline - routed_actual
```

Label this `price-normalized baseline`; never call it an observed baseline. It does not
model different output length, tokenizer behavior, or cache reuse. Once enough paired
data exists, reports may add a second class-specific estimate derived from observed
A/B token ratios, while retaining the simple calculation for transparency.

For A/B tasks, both branch costs are observed:

```text
paired_delta       = orchestrator_branch_actual - routed_branch_actual
experiment_overhead = cost of the branch that would not otherwise run
net_spend_impact   = estimated_operational_savings - experiment_overhead
```

Keep paired evidence separate from normalized estimates.

### 14.3 Storage and reports

Add raw decision/invocation tables plus an idempotent UTC daily rollup. Raw rows follow
the existing bounded state retention; complete days are folded before pruning. Reports
support:

- actual tokens/cost by model, provider, effort, class, and scope;
- price-normalized baseline and estimated savings;
- A/B paired delta and experiment overhead;
- route-switch count and cached-input share;
- shadow proposal distribution;
- confidence, abstention, fallback, and override rates;
- latency and model-artifact failures;
- unknown-price and unknown-usage coverage.

The headline shows all three money numbers:

```text
Actual $12.40 · baseline ~$19.10 · estimated savings ~$6.70
A/B overhead $1.25 · net spend impact ~$5.45
```

Tildes mark counterfactual values. Observed A/B values and calculated costs from
reported tokens do not use a tilde.

### 14.4 App-server-hosted web UI

The app-server owns the reporting backend and static web application. This keeps
report definitions, database access, and cost calculations in one process instead of
reimplementing them in the TUI.

Opening flow:

```text
/model-router report 30
  └─► modelRouterReport/open
        └─► app-server starts/reuses listener and returns capability URL
              └─► TUI OpenUrlInBrowser
                    └─► static UI + authenticated bounded JSON queries
```

The report HTTP server is lazy and independent of the app-server's JSON-RPC transport.
It binds to `127.0.0.1:0` by default, even when the TUI uses an embedded, stdio, or Unix
socket app-server. Reusing the existing WebSocket listener is insufficient because
those modes may not have a TCP listener, and the WebSocket listener's current
origin-rejection policy is intentionally hostile to browser clients.

Proposed experimental app-server v2 methods:

- `modelRouterReport/open`: validate the date/filter request, start or reuse the
  report listener, and return a short-lived capability URL.
- `modelRouterReport/read`: return the same versioned, bounded aggregate DTO for
  non-browser clients.

The web application is shipped with Xedoc and served without a CDN or runtime package
manager. Prefer a small static HTML/CSS/JavaScript application with inline SVG charts.
If assets are read at compile time, add them to the app-server Bazel target's
`compile_data`.

The first UI should contain:

- summary cards for actual cost, price-normalized baseline, estimated savings, A/B
  overhead, and net spend impact;
- daily actual-versus-baseline and token-consumption charts, with breakdowns by task
  class, route, and root/subagent scope;
- A/B outcome, cost-delta, and latency tables;
- confidence, abstention, fallback, override, cache-share, and route-switch views;
- a bounded recent-decision table with class, proposed/effective route, disposition,
  cost, and fallback reason;
- unknown-price/usage coverage so incomplete accounting cannot look like free inference.

All filtering and pagination are server-bounded. Raw prompts and model outputs are not
available through the report API.

For a local app-server, the capability token is placed in the URL fragment so it is not
sent in the initial HTTP request, then used by JavaScript as an authorization header
for data requests. Tokens are random, short-lived, report-read-only, and scoped to one
app-server process. Responses disable caching and restrictive CSP/referrer headers
prevent token or data leakage.

For a remote app-server, return an explicitly configured browser-reachable HTTPS base
URL, never the server's `127.0.0.1`. Without one, the slash command reports the
limitation while `modelRouterReport/read` remains available to remote clients.

## 15. Failure, privacy, and safety

- Routing failure never prevents task execution when a valid fallback route exists.
- Missing or corrupted embedding artifacts are visible and disable application of
  automatic routes; shadow/error records still explain the failure.
- No model artifact is fetched implicitly during a turn.
- Raw prompts never enter the policy or metrics database. Calibration reads rollouts
  locally and writes vectors, hashes, opaque source IDs, and aggregate reports.
- The report listener binds to loopback by default, uses short-lived read-only
  capabilities, rejects cross-origin data requests, and never exposes raw prompts or
  outputs.
- Non-loopback report hosting requires an explicit HTTPS public URL and the
  app-server's authenticated deployment boundary; it is never inferred from a
  WebSocket bind address.
- Prototype count, task input, decision events, reports, and TUI history are hard
  capped.
- Explicit user choices and safety policy outrank cost optimization.
- High-impact classes can declare a minimum model capability or effort floor.
- A provider/model route must be validated as a pair to avoid routing to the wrong
  provider when slugs overlap.
- Root model changes are recorded because they may invalidate prompt-cache reuse.
- A/B write tasks require isolation; inability to isolate is a hard skip, not a
  warning.

## 16. Delivery stages

Each stage should be independently reviewable and remain below the repository's change
size guidance.

### Stage 1 — Calibration spike

- Port the privacy-preserving extractor and benchmark to a reproducible calibration
  command without persisting raw prompt copies.
- Benchmark a deterministic linear classifier over the selected Arctic embeddings.
- Verify model loading and deterministic inference on Linux, macOS, and Windows.
- Pin and checksum the embedding artifact; decide thresholds and packaging from
  evidence.

Exit: a reviewable calibration report and proposed `model-router.toml`.

### Stage 2 — Router core and subagents

- Add `xedoc-model-router`, policy parsing/validation, local artifact loading, and
  deterministic decisions.
- Integrate subagent direct and shadow modes.
- Emit TUI decisions and raw accounting rows.

Exit: `off`, `shadow-subagents`, and `subagents` work end to end.

### Stage 3 — Root turns and full modes

- Route before root `TurnContext` creation.
- Reuse the existing provider/model switch path.
- Surface route switches and cache effects.

Exit: `shadow-full` and `full` work end to end without mutating prior history.

### Stage 4 — Reporting

- Attribute provider-reported usage to decisions.
- Add configured baseline repricing, unknown-price handling, daily rollups, TUI status,
  and bounded report queries.
- Add the app-server-hosted static web UI, lazy loopback listener,
  `modelRouterReport/open|read`, capability authorization, and `/model-router report`
  browser launch.

Exit: actual spend, normalized baseline, estimated savings, and route quality signals
are auditable from the browser UI.

### Stage 5 — Orchestration skill and A/B

- Add the scheduling-focused orchestration skill.
- Add one-turn A/B state, paired leaf spawns, write isolation, result preference, and
  experiment-overhead reporting.

Exit: paired route tuning works for read-only and isolated implementation tasks.

### Stage 6 — Tuning loop

- Generate reviewable policy recommendations from paired and drift data.
- Support atomic activation and rollback of policy revisions.

Exit: class-to-route mappings can improve without code changes or silent self-modifying
policy.

## 17. Risks and unresolved decisions

- The selected Arctic embedding plus prototype classifier is not accurate enough for
  unrestricted routing. A calibrated linear head and conservative fallback gates are
  required before direct modes are enabled.
- The historical corpus is selection-biased. It establishes taxonomy and frequency,
  not causal route quality.
- Root route switching may cost more in uncached input than it saves on output.
  Provider-reported cached-token accounting must be part of tuning.
- A/B doubles eligible subagent inference and can duplicate side effects. Leaf
  enforcement and write isolation are mandatory.
- Model prices change. Capturing price revisions preserves historical calculations,
  while reports may optionally reprice old usage under current rates.
- The sidecar artifact adds release/install complexity. The calibration spike must
  compare bundled sidecar delivery against a checksum-pinned explicit download.
- Initial quality thresholds and minimum capability floors need calibration evidence;
  they should not be guessed in implementation.
- Remote report access needs an explicitly advertised HTTPS URL; local loopback cannot serve a browser on another machine.
