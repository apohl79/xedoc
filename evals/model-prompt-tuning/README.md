# Model prompt tuning evaluation

This standard-library Python harness compares prompt variants through
`xedoc exec --json`. It copies each fixture into an isolated temporary
workspace, records raw JSONL and stderr, runs deterministic checks without a
shell, and reports aggregate and paired-baseline metrics.

```sh
<python> run.py example-manifest.json --dry-run
<python> run.py example-manifest.json --output ./results
```

Use a Python 3.9+ executable for `<python>` (`python3` on Unix-like systems or
`py -3` on Windows).

The example does not run by itself. Before a real run, replace its provider and
model placeholders with exact IDs from the configured catalog (available from
`xedoc debug models`). A manifest may list any mix of configured OpenAI,
Anthropic, Gemini, DeepSeek, or other providers.

`schema_version` is currently `1`. The manifest contains:

- `baseline_variant`: the control variant ID.
- `repeats`: repetitions of every model/variant/scenario combination.
- `judge_model`: the fixed provider, model, and effort used for isolated LLM
  judgment when any scenario defines `judge`.
- `models`: stable result ID plus explicit `provider`, `model`, and `effort`.
- `variants`: stable result ID and optional `instructions_file`. Omitting the
  file uses the model's resolved instructions; specifying it replaces those
  base instructions for that variant.
- `scenarios`: fixture workspace, user prompt, run/check timeouts, and checks
  expressed as argv arrays. The exact `{python}` argument expands to the
  interpreter running the harness.
- `scenarios[].judge.files`: the explicit, non-sensitive workspace files the
  judge may inspect after the candidate run.

Real runs require working Xedoc credentials and network access. The output
directory receives one raw JSONL and stderr file per run plus `results.json`.
Set `XEDOC_EVAL_BINARY` to the exact rebuilt Xedoc executable before a real
run; the harness rejects a PATH fallback and records the resolved binary path.
The result records completion and check success; input, cached-input,
cache-write, output, and reasoning tokens; duration; completed item and tool
counts; error/malformed-line counts; and agent-message characters. Aggregates
group runs by model and variant, while paired deltas compare matching
model/scenario/repetition runs with the baseline.

When a scenario defines `judge` metadata, the harness runs a second, isolated
`xedoc exec --json` session using the manifest's required `judge_model`. It
copies a bounded snapshot of only the scenario-declared `judge.files`,
deterministic-check outcomes, reference facts, and rubric into a fresh judge
workspace. It deliberately excludes candidate final messages and raw check
arguments. The judge writes a schema-validated `JUDGMENT.json`; its raw JSONL,
stderr, verdict, and bounded evaluation input are saved beside the candidate
artifacts. The snapshot omits sensitive paths and credential-shaped content.
The aggregate
reports judge-run success, pass rate, and average overall score. Candidate
prompts do not include reference facts or judge criteria. Both turns inherit
`XEDOC_INTER_AGENT_TRACE_FULL=1`, so sampling logs contain the exact backend
requests Xedoc issued for both the candidate and the judge.

Results are noisy and statistical. Use several repetitions, compare identical
model/provider/effort settings, and treat lower token or response length as an
efficiency signal only when the behavioral checks still pass. Checks execute
trusted manifest commands on the temporary fixture copy.

The checked-in manifest contains 20 deliberately different scenarios distilled
from session-history work patterns: planning, decomposition, delegation,
subagent synthesis, implementation, debugging, refactoring, documentation,
configuration, migrations, security, performance, research, reviews, and
release verification. `tags` are analysis labels; they do not change the
request sent to the model. The harness still measures behavioral outcomes
through the scenario checks and records tool-call counts, so orchestration
changes can be compared without treating a tool call alone as success.

`noninteractive-manifest.json` additionally contains two summary scenarios:

- `summarize-complex-design` is a sanitized version of the model-prompt
  overhaul design. It tests whether the agent preserves precedence, ownership,
  and validation constraints in a complex technical document.
- `summarize-research-synthesis` is a sanitized synthesis of a longer
  architecture-research session. It tests whether the agent separates evidence,
  decisions, causal relationships, and open questions.

Their user prompts only ask for a summary and name its output file. The
`judge` metadata is deliberately outside the candidate-model request: it
supplies the isolated LLM judge with reference facts and a clarity rubric,
including simple language, without teaching that rubric to the candidate model.

## Exact backend replay

For prompt-only backend experiments, use `replay_request.py` with a full
`XEDOC_INTER_AGENT_TRACE_FULL=1` JSONL trace:

```sh
python3 replay_request.py /path/to/sampling-trace.jsonl \
  --endpoint https://api.openai.com/v1/responses \
  --prompt-file prompts/candidate.txt \
  --body-output candidate-request.json
python3 replay_request.py /path/to/sampling-trace.jsonl \
  --endpoint https://api.openai.com/v1/responses \
  --prompt-file prompts/candidate.txt \
  --send
```

The replay starts from the captured request body and changes only the first
single-text developer message when `--prompt-file` is supplied. It preserves
the model, tools, reasoning settings, input history, and all other JSON fields.
Without `--send`, it is an offline fixture generator. The script rejects
unknown transports and supports both the HTTP and WebSocket envelopes emitted
by Xedoc.

The sent JSON uses compact UTF-8 serialization and preserves the captured
request structure. This guarantees byte-equivalent JSON semantics for the
captured request body, not equivalent authentication headers, TLS, or
provider-side session state. It replays one sampling request; a multi-turn
behavioral evaluator must implement the provider's response/tool loop and
derive each subsequent request from the captured envelope.
Keep the Xedoc E2E trace assertion as the source-of-truth check that the fixture
was produced from the request Xedoc actually sent.
