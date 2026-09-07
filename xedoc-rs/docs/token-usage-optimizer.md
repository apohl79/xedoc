# Token Usage Optimizer — Design

Status: Implemented (Phases 0–F; documentation pass)
Owner: fork maintainers
Scope: `xedoc-rs` (in-process; no proxy, no ML)

## 1. Summary

Tool outputs (shell, `apply_patch`, MCP results, subagent results) are the largest
uncontrolled contributor to input tokens in an agent session. Today xedoc bounds them
with a size-only head/tail truncation
(`xedoc-rs/utils/output-truncation/src/lib.rs`) that neither understands the payload
nor lets the model recover what was cut.

This design adds a **content-type-aware reduction stage** that runs once, at tool-output
ingestion, replacing dumb truncation with shape-specific reducers (whitespace/ANSI
normalisation, repeated-line dedup, JSON array crushing, signal-preserving log/diff
truncation) and makes every reduction **reversible** by spilling the original to disk.
The whole system is toggleable at runtime via `/token-usage-optimizer` and records
per-reduction metrics so effectiveness (tokens/cost saved, false positives) can be
measured and iterated on.

## 2. Goals and non-goals

Goals

- Reduce model-visible tool-output tokens without losing task-relevant information.
- Command-agnostic: reducers key on *payload shape*, never on the command that produced it.
- Deterministic, sub-millisecond, pure Rust, no network, no external binaries.
- Reversible: the model can always fetch the original by path.
- Toggleable and configurable at runtime (`/token-usage-optimizer`), persisted in config.
- Observable: per-reduction records, session and lifetime aggregates, estimated cost
  savings, and effectiveness signals (how often the model needed the original).
  Records capture the active model slug and input price at ingestion time so historical
  attribution remains stable when pricing configuration changes.
- Comply with the model-visible-context rules in `AGENTS.md`: no history rewrite,
  bounded items, cache-friendly.

Non-goals (for now)

- No proxy layer, no wire-level compression, no output shaping (verbosity steering,
  effort routing).
- No ML/learned compression (Headroom's `kompress` model).
- No per-command output filters (rtk's approach): brittle across tool versions, silent
  loss, shell-only coverage.
- No AST-based code compression (would pull in tree-sitter; revisit later).
- No compression of user or assistant messages; history-level savings remain the job of
  compaction (`xedoc-rs/core/src/compact.rs`).

## 3. Background

### 3.1 What exists in xedoc today

| Concern | Where | Notes |
|---|---|---|
| Tool output formatting for the model | `xedoc-rs/core-tool-output/src/output.rs` | `ExecCommandToolOutput` adds `Wall time`/`Output:` header, then truncates at `policy * 1.2`. Knows the tool that produced the output. |
| Size-only truncation | `xedoc-rs/utils/output-truncation/src/lib.rs` | Head/tail by byte budget; header `Warning: truncated output (original token count: N)`. |
| Safety-net truncation on record | `xedoc-rs/core-context-manager/src/history.rs` `record_items` → `process_item` → `truncate_function_output_payload` | Applies to every `FunctionCallOutput` regardless of tool. |
| Prompt assembly | `xedoc-rs/core-context-manager/src/history.rs` `for_prompt` → `normalize_history` | Sampling-time seam; touching items here invalidates prompt cache. |
| Token estimate | `xedoc_utils_string::approx_token_count` | Bytes / 4 heuristic. |
| Model prices | `ModelTokenPrices` via `xedoc-rs/core-config/src/config/mod.rs` | Input / cached input / output per 1M tokens. |
| Spill oversized text to disk | `xedoc-rs/hooks/src/output_spill.rs` | Hook outputs only; pattern to reuse. |
| Runtime feature toggles | `xedoc-rs/features/src/lib.rs` `Feature`, `Features::set_enabled` | e.g. `Feature::Personality`. |
| Slash commands | `xedoc-rs/tui-completion/src/slash_command.rs`, dispatch in `xedoc-rs/tui-chatwidget/src/chatwidget/slash_dispatch.rs` | |
| Persistent state DB | `xedoc-rs/state` (SQLite, migrations) | Home for metrics rows. |
| Metrics export | `xedoc-rs/otel/src/metrics/client.rs` `counter`/`histogram` | |

### 3.2 Prior art

- **rtk** — shell-side, per-command filters, hook rewrites `git status` →
  `rtk git status`. Fast and deterministic but brittle, lossy, shell-only.
- **Headroom** — wire-side proxy with a *content router* (JSON crusher, code
  compressor, ML text model), reversible via a local cache, plus cache alignment. The
  router idea is adopted here; the proxy, ML, and sampling-time history rewriting are
  not.

## 4. Architecture

```
tool handler ──► ToolOutput (core-tool-output)
                    │  raw text / content items, tool name, call_id
                    ▼
            ┌──────────────────────────────┐
            │ xedoc-tool-output-reduce      │   new crate
            │  Router: detect payload kind  │   json | log | diff | table | code | prose | binary-ish
            │  Reducers (ordered pipeline)  │   normalize → dedup → kind-specific → budget fit
            │  Spiller: original → disk     │   $XEDOC_HOME/tool_outputs/<thread>/<call_id>.txt
            │  Report: ReductionRecord      │   bytes/tokens in/out, kind, reducers, duration
            └──────────────────────────────┘
                    │ reduced payload + header
                    ▼
        ContextManager::record_items (safety-net truncation unchanged)
                    │
                    ▼
        MetricsSink ──► state SQLite table ──► /token-usage-optimizer stats, /status
                   └──► OTel counters/histograms
```

### 4.1 Placement: ingestion, not sampling

Reduction runs **once**, when the tool output is turned into a `FunctionCallOutput`,
inside `core-tool-output` (which knows the tool and call id). The reduced item is then
recorded and never touched again. Consequences:

- History stays append-only → no prompt-cache invalidation, complies with "no history
  rewrite".
- Rollouts persist the reduced item; resume/fork behave identically with the feature
  on or off later.
- `ContextManager::process_item` truncation stays as the hard cap safety net.

Sampling-time aging (compress older items harder in `for_prompt`) is deliberately
deferred to a gated phase (§6, Phase 6) because it is a history rewrite with a
measurable cache-miss cost.

### 4.2 Crate: `xedoc-tool-output-reduce`

New workspace crate (not `xedoc-core`), depends only on `xedoc-protocol`,
`xedoc-utils-output-truncation`, `xedoc-utils-string`, `serde_json`.

Public API (small):

```rust
pub struct ReductionConfig { level: ReductionLevel, budget: TruncationPolicy, spill_dir: Option<AbsolutePathBuf> }
pub enum ReductionLevel { Off, Conservative, Balanced, Aggressive }
pub struct ReductionInput<'a> { tool_name: &'a str, call_id: &'a str, text: &'a str }
pub struct ReductionOutput { text: String, record: ReductionRecord }
pub fn reduce(input: ReductionInput<'_>, config: &ReductionConfig) -> ReductionOutput;
```

Internals (private modules, ≤500 LoC each): `router`, `normalize`, `dedup`, `json`,
`log`, `diff`, `budget`, `spill`, `record`.

Payload-kind detection is heuristic and cheap: leading `{`/`[` + successful
`serde_json` parse → `json`; `diff --git` / `@@` hunks → `diff`; high ratio of
timestamp/level-prefixed lines → `log`; otherwise `prose`. Unknown kinds get only the
normalise/dedup/budget stages.

Reducers (all deterministic, all idempotent):

| Reducer | Applies to | Behaviour | Level |
|---|---|---|---|
| `normalize` | all | strip ANSI, collapse runs of blank lines, trim trailing whitespace, cap line length with `…[+N chars]` | Conservative+ |
| `dedup` | all | collapse consecutive identical lines → `line  [×N]`; collapse near-identical lines differing only in digits/hex (progress bars, timestamps) at Balanced+ | Conservative+ |
| `json` | json | arrays > N items → keep first k / last k, keep items that are outliers (extra keys, error-ish values), emit `… N more items, same shape {keys}` | Balanced+ |
| `log` | log | keep all lines matching signal patterns (`error|fatal|panic|fail|warn|exception|traceback`) plus ±2 context; window the rest | Balanced+ |
| `diff` | diff | drop `index`/`similarity` headers, keep file headers and hunks, cap hunk context lines | Aggressive |
| `budget` | all | final head/tail fit to the byte budget (existing `truncate_text`) | always |

Header emitted to the model when anything was reduced:

```
Note: output reduced by xedoc (json: 5,812 → 940 tokens; 3 of 412 items shown).
Full output: ~/.xedoc/tool_outputs/<thread>/<call_id>.txt
```

The header is bounded, stable in shape, and tells the model exactly how to recover.

### 4.3 Reversibility (spill)

- Originals are written to `$XEDOC_HOME/tool_outputs/<thread_id>/<call_id>.txt` only
  when a reduction actually changed the payload.
- Retention: prune on session start, keep last N MiB / M days (config).
- The model reads originals via ordinary shell tools; reading a spill path is detected
  (path prefix match in the shell handler) and counted as a **retrieval** — the primary
  false-positive signal (§5).
- Spilled files are outside the workspace and never included in patches.

### 4.4 Control surface

- Config (`config.toml`, `[token_usage_optimizer]`):

  ```toml
  [token_usage_optimizer]
  enabled = true              # master switch
  level = "balanced"          # conservative | balanced | aggressive
  spill_retention_days = 7
  spill_max_mib = 256
  ```

- Feature flag `Feature::TokenUsageOptimizer` mirrors `enabled`, so the existing
  `Features::set_enabled` + config-write path used by `/personality` handles runtime
  toggling and persistence.
- Slash command `/token-usage-optimizer`:
  - no args → status panel (enabled, level, session savings, lifetime savings)
  - `on` / `off` → toggle (persisted, effective for the next tool call; already recorded
    items are untouched by design)
  - `level <conservative|balanced|aggressive>`
  - `stats` → detailed insights view (§5)
  - `report [days]` → durable day-by-day savings report (default 90 days, bounded to
    366 days)
  - `reset-stats`
- App-server: expose the same as `tokenUsageOptimizer/read` and
  `tokenUsageOptimizer/write` in v2 (follow app-server rules in `AGENTS.md`), so the
  IDE/daemon clients can toggle it too. Config RPC payloads keep snake_case.
- `Op`/`EventMsg`: a `TokenUsageOptimizerUpdated` notification carries the new
  state to clients after toggles.

### 4.5 Invariants

- Applied at most once per item; the reduced item is immutable afterwards.
- Never enlarges a payload (falls back to the input if a reducer would).
- Output is always ≤ the existing truncation budget; hard cap unchanged.
- Off ⇒ byte-identical to today's behaviour (only truncation + header).
- No tool-specific logic; the `tool_name` is used only for metrics tagging and to skip
  tools whose output is already structured for the model (e.g. `tool_search`).
- Works on Linux/macOS/Windows; spill paths go through `AbsolutePathBuf`.

## 5. Metrics and insights

### 5.1 Per-reduction record

```rust
pub struct ReductionRecord {
    thread_id: ThreadId, turn_id: String, call_id: String,
    tool_name: String, kind: PayloadKind, level: ReductionLevel,
    reducers_applied: Vec<ReducerId>,
    bytes_in: u64, bytes_out: u64,
    est_tokens_in: i64, est_tokens_out: i64,
    duration_us: u64,
    spilled: bool,
    model_slug: Option<String>,
    input_price_per_1m: Option<f64>,
    recorded_at: i64,
}
```

Plus an event row when a spill path is read back: `(call_id, retrieved_at)`.

Storage: new tables `tool_output_reductions` and `tool_output_retrievals` in the
`xedoc-rs/state` SQLite DB via a migration. Writes are fire-and-forget through the
existing `StateRuntime` so the tool path never blocks on the DB.

Input prices are captured with each row. If a provider does not publish a USD input
price, the price is `NULL`; token savings remain valid, while cost is unknown rather
than falsely reported as free.

### 5.2 Derived insights

| Metric | Definition | Why |
|---|---|---|
| Tokens saved (est.) | Σ (est_tokens_in − est_tokens_out) | headline number; labelled *estimated* (bytes/4) |
| Cost saved (est.) | tokens saved × input price of the model active at that turn (`ModelTokenPrices`), summed | USD estimate; `n/a` when a row has no price |
| Reduction ratio by kind / reducer / tool | out ÷ in grouped | which reducers earn their keep |
| Retrieval rate | retrievals ÷ spilled reductions | **false-positive proxy**: the model needed what we cut |
| Retrieval rate by kind / reducer | same, grouped | tells which reducer to tune |
| Re-run rate | same command re-executed within 2 turns after a reduction | secondary false-positive proxy |
| Latency p50/p99 | duration_us | must stay sub-ms |
| Coverage | reductions ÷ tool outputs | how much traffic we even touch |

Actual tokens saved are counterfactual; we report bytes/4 estimates. Cost is computed
as `(estimated_tokens_in - estimated_tokens_out) × captured_input_price_per_1m / 1e6`.
Rows without a captured USD price contribute zero to aggregate cost and are surfaced as
unknown (`cost n/a`) in per-model and per-reduction views; they are not treated as free.

### 5.3 Surfaces

- `/token-usage-optimizer` → compact panel: state, level, session saved (tokens, ~cost),
  lifetime saved, retrieval rate.
- `/token-usage-optimizer stats` → grouped insights by kind, reducer, tool, and model,
  with top reductions and retrieval counts. Dollar values are estimates; model groups
  with unknown pricing display `cost n/a`.
- `/token-usage-optimizer report [days]` → deterministic daily report backed by the
  durable rollup. Complete UTC days are folded on demand; today's values are marked
  `partial` and read from raw rows. The report is bounded to 366 days.
- `/status` → one extra line: `Token optimizer: on (balanced) · saved ~12.4k tokens
  this session`.
- OTel: `xedoc.tool_output.reduction.tokens_in/out` counters,
  `xedoc.tool_output.reduction.duration_us` histogram, `…retrieval` counter, tagged
  with `kind`, `reducer`, `level`, `tool`.
- `xedoc debug token-optimizer export --json` for offline analysis.

### 5.4 Durable report and retention

Raw reduction rows remain the short-horizon source and are pruned by the existing
30-day age/row-cap retention job. Migration `0048` adds an unpruned
`tool_output_reduction_daily` table keyed by UTC day and model plus a high-water mark.
The report folds complete days on demand in one idempotent transaction; today's UTC day
is included only as a clearly marked partial view from raw rows. `reset-stats` clears
raw session/lifetime metrics but intentionally preserves the durable rollup. Use
`xedoc report-token-savings --purge` to explicitly delete the rollup and reset its
high-water mark.

Because folding is on demand, run `xedoc report-token-savings` (or the equivalent
in-TUI report/app-server request) at least once within each 30-day raw-retention
window if long-term reporting is required. There is no background scheduler.

The top-level CLI command accepts `--days N` (default 90, maximum 366), or explicit
UTC `--since YYYY-MM-DD` and `--until YYYY-MM-DD` dates, plus `--model SLUG`.
`--json` emits a bounded versioned object with `days`, `byModel`, `tokensSaved`, and
`costSavedUsd` fields for dashboards. `--purge` explicitly deletes the durable rollup.
CLI output is deterministic for a fixed database: days sort ascending, then models
sort ascending.

## 6. Phases

Each phase is an independently reviewable PR (target ≤ 500 changed lines for logic
phases, ≤ 800 for mechanical ones), lands behind the master switch, and ships with
tests per `AGENTS.md`. Phases 1 and 2 are intentionally *measure-first*: baseline data
before any content is changed.

### Phase 0 — Skeleton and control surface

- New crate `xedoc-tool-output-reduce` with `reduce()` as a pass-through.
- `[token_usage_optimizer]` config, `Feature::TokenUsageOptimizer`, schema update
  (`just write-config-schema`).
- `/token-usage-optimizer` with `on|off|level|status` (no stats yet); persistence via the
  `/personality` pattern.
- App-server `tokenUsageOptimizer/read|write` (v2) + schema fixtures.
- Tests: config round-trip, slash command dispatch, TUI snapshot of the status panel.
- Exit: toggling works end to end; behaviour byte-identical to today.

### Phase 1 — Metrics pipeline and baseline

- `ReductionRecord`, state DB migration, `StateRuntime` sink, OTel emission.
- Instrument the *existing* truncation path so we get baseline data: how many outputs
  are truncated, sizes, by tool.
- `/token-usage-optimizer stats` v1 (session aggregates) and `/status` line.
- Tests: migration test, sink unit tests (`*_tests.rs`), TUI snapshot for stats.
- Exit: after a day of use, we know what tool output actually looks like in practice.

### Phase 2 — Reversible truncation (spill + retrieval tracking)

- Spill originals when truncation/reduction changes the payload; new header with path.
- Retention pruning on session start.
- Retrieval detection in the shell handler (path prefix) → `tool_output_retrievals`.
- Tests: `core/suite` integration test — oversized output → header with path, reading
  the path is recorded as a retrieval.
- Exit: nothing is ever lost; the false-positive signal exists before we start cutting
  smarter.

### Phase 3 — Normalise + dedup reducers (Conservative level)

- Router skeleton, `normalize` and `dedup` reducers, `budget` stage.
- Default level `conservative` once this lands.
- Tests: unit tests per reducer (idempotence, never-enlarge), integration test that a
  noisy build log is reduced with counts, snapshot of the model-facing header.
- Exit: low-risk savings on every tool, metrics show ratio per reducer.

### Phase 4 — JSON crusher (Balanced level)

- `json` kind detection and array crushing with shape summary and outlier retention.
- Applies to MCP tool results too (`core-tool-output/src/mcp_result.rs`).
- Tests: unit tests on nested arrays / heterogenous shapes / outliers; integration test
  with an MCP mock returning a 500-item array.
- Exit: retrieval rate for `json` reductions stays below an agreed threshold (proposal:
  < 5 %).

### Phase 5 — Log and diff reducers (Balanced / Aggressive)

- `log` signal-preserving windowing; `diff` header stripping and context capping.
- Tests: fixtures with `FATAL` buried at item 67 must survive byte-for-byte; git diff
  fixture keeps every hunk header.
- Exit: same retrieval-rate gate as Phase 4, per kind.

### Phase 6 — Insights v2 and tuning loop

- Effectiveness views: retrieval and re-run rates by kind/reducer/tool, top reductions,
  export command.
- Use the data to re-tune thresholds; promote default level if the numbers support it.
- Exit: the team can answer "is this helping and where does it hurt" from the TUI.

### Phase 7 (gated decision) — Sampling-time aging

- Optional: in `for_prompt`, replace tool outputs older than K user turns with their
  header + summary line, freezing each item once so it is rewritten at most once.
- Requires an explicit decision to relax the "no history rewrite" rule for this case,
  and a measurement plan for cache-hit impact vs. savings.
- Not scheduled; documented so the trade-off is visible.

## 7. Testing strategy

- Reducers: unit tests in dedicated `*_tests.rs` files, comparing whole outputs; a
  property-style check that `reduce(reduce(x)) == reduce(x)` and
  `len(reduce(x)) <= len(x)`.
- Agent behaviour: integration tests under `core/suite` with `test_xedoc` and the
  `responses` mocks, asserting the exact `function_call_output` body the model sees.
- UI: `insta` snapshots for the status panel, stats view, and `/status` line.
- Cross-platform: spill paths and pruning exercised on Windows in CI.

## 8. Risks and open questions

- **Estimate accuracy**: bytes/4 is crude; percentages are reliable, absolute numbers are
  not. Mitigation: label as estimated, show observed input-token trend alongside.
- **Model confusion**: models may not act on the "Full output:" hint. Mitigation:
  measure retrieval rate, iterate on header wording; consider a one-line note in the
  system prompt fragment (must be a `core/context` fragment per `AGENTS.md`).
- **Hard cap interplay**: the reduce header plus body must fit the existing budget;
  `budget` stage runs last and accounts for header length.
- **Dedup false merges**: digit-insensitive near-dedup can merge distinct lines
  (e.g. two different test names differing only in a number). Gate behind Balanced+,
  keep exact-dedup at Conservative.
- **Disk growth**: spill retention defaults and pruning must be conservative; report
  spill directory size in `stats`.
- Open: should reduction also apply to subagent final answers and hook additional
  context? Proposal: not in Phases 0–6.
- Open: expose reduction records via app-server for IDE dashboards? Defer until Phase 6
  shows what is worth exposing.
