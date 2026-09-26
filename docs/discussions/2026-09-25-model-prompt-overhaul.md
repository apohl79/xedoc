# Model prompt overhaul

This document specifies a single, shared base prompt for every model Xedoc
runs, a single place that builds it, and storage that stops freezing prompt
copies into user state. The goal is shorter, sharper instructions that are true
for every provider, tool set, and permission mode, and that change in one place.

## Goals

- One built-in base prompt that is the floor for every provider and model,
  with a hard size cap.
- Base-prompt content that holds regardless of tools, sandbox, approval policy,
  collaboration mode, and provider. Conditional guidance lives with the
  component that knows the condition.
- One resolution function that decides the effective instructions for a model,
  used by every path that lists models or starts a session.
- Model-specific customization only through explicit user choices: prompt files
  in `~/.xedoc/prompts/` or the opt-in use of provider-supplied instructions.
- No prompt text in `~/.xedoc/models.json` or the bundled `models.json`, so
  prompt updates reach existing installs.
- Every model gets a real `apply_patch` tool that carries the patch format, so
  the prompt never tells a model to call a tool it does not have.
- Delete unused and duplicate prompt files.

## Non-goals

- Rewriting developer-message fragments (permissions, approval policy,
  collaboration mode, skills catalog, multi-agent, goals). The base prompt stops
  duplicating them; their own content is a separate effort.
- Compaction, review, and auto-review policy prompts.
- New app-server API fields. The only new configuration key is the opt-in
  switch for provider-supplied instructions.
- Built-in per-model prompt variants. Model-specific text comes only from user
  prompt files or, when enabled, the provider's catalog.

## Current state

### Where prompt text comes from

| Models | Source | Size |
| --- | --- | --- |
| Anthropic, Google, DeepSeek, unknown slugs, registry `*` templates | `xedoc-rs/models-manager/prompt.md` plus a generated `# Model Identity` footer (`models-manager/src/model_info.rs:182`) | 20.7 KB, ~5.2k tokens |
| `gpt-6-sol`, `gpt-6-luna`, `gpt-5.6-*` | inline in `xedoc-rs/models-manager/models.json` | 17.7 KB, ~4.4k tokens |
| `gpt-5.5` | inline in `models.json` | 19.7 KB, ~4.9k tokens |
| `gpt-5.4`, `xedoc-auto-review` | inline in `models.json` | 12.9 KB, ~3.2k tokens |
| `gpt-5.4-mini` | inline in `models.json` | 11.1 KB, ~2.8k tokens |
| `gpt-5.2` | inline in `models.json` | 21.5 KB, ~5.4k tokens |

Each `models.json` entry stores the prompt twice: `base_instructions` and a
near-identical `model_messages.instructions_template`. Only one is sent
(`protocol/src/openai_models.rs:485`). OpenAI's Codex `/models` response also
carries its own, newer versions of these prompts; see step 5 below.

### Resolution order today

1. Config `base_instructions` or `model_instructions_file`
   (`core-config/src/config/mod.rs:3665`).
2. Stored session metadata on resume (`core/src/session/mod.rs:746`).
3. `~/.xedoc/prompts/<provider>/<model>.md`, then `~/.xedoc/prompts/<model>.md`
   (`models-manager/src/registry_manager.rs:205`). These are applied in memory
   and clear `model_messages` (`registry_manager.rs:48`, `:160`).
4. The user registry `~/.xedoc/models.json`
   (`models-manager/src/registry.rs:166`). It holds a full `ModelInfo`,
   including prompt text, for every configured model and each provider's `*`
   template. Almost all of that text is copied built-in text (see
   [How prompt text enters the registry](#how-prompt-text-enters-the-registry)).
5. Catalog text, from a remote `/models` response or a config `model_catalog`.
   - **Registry providers:** the list and the session lookup both use the
     registry entry or template, so catalog text is never used. OpenAI's Codex
     backend does send full instructions (in this machine's cache,
     `~/.xedoc/models_cache/openai/models_cache_v2.json` fetched 2026-09-25,
     `gpt-5.5` has 21,459 bytes starting "You are Codex…"), but they are
     fetched, cached, and ignored.
   - **Providers without a registry entry:** the model list replaces each entry
     with the Xedoc fallback (`registry_manager.rs:190`), while the session
     lookup keeps the catalog entry as-is (`registry_manager.rs:154`,
     `.unwrap_or(discovered)`). Catalogs in Xedoc's own format, remote
     (`model-provider/src/models_endpoint.rs:150`) or from config
     (`StaticModelsManager`, `model-provider/src/provider.rs:413`), are used
     whole when they match (`models-manager/src/manager.rs:909`), including
     `base_instructions` and `model_messages`. Nothing requires that text to be
     non-empty (`protocol/src/openai_models.rs:396`).
   - **Plain id lists** (OpenAI-compatible `/models`, Gemini) are turned into
     the Xedoc fallback prompt with the identity footer
     (`models_endpoint.rs:168`, `model-provider/src/gemini_models_endpoint.rs:89`).
   - **Unknown slugs** get `BASE_INSTRUCTIONS` without the footer
     (`manager.rs:917`, `model_info.rs:127`).

Rendering applies personality through `{{ personality }}` in
`instructions_template` (`protocol/src/openai_models.rs:485`) and, for
`personality = none`, strips a `# Personality` H1 section
(`models-manager/src/model_info.rs:77`). When a template exists it always
wins over `base_instructions` (`openai_models.rs:486`).

### How prompt text enters the registry

1. **Creation** (`registry.rs:121`, via `default_registry`):
   - OpenAI models: `openai_defaults` (`models-manager/src/registry_defaults.rs:79`)
     copies each bundled `models.json` entry whole, including
     `base_instructions` and `instructions_template`.
   - Named Anthropic, Google, and DeepSeek models: `provider_defaults` stores
     `model_info_from_provider_catalog_slug` output (`model_info.rs:182`),
     which is `prompt.md` plus the identity footer.
   - Each provider's `*` template: `provider_template`
     (`registry_defaults.rs:144`) stores `BASE_INSTRUCTIONS` with no footer,
     and `configured_model` (`registry.rs:173`) does not add one. Unlisted
     models on registry providers get no identity line.
2. **Migration** (`registry.rs:265`) adds missing providers and models from the
   same built-in copies and never changes existing entries.
3. **Custom providers on first model-manager update:** `ensure_provider_config`
   (`app-server/src/request_processors/model_manager_processor.rs:370`) writes
   `prompt.md` plus the footer.
4. **Model-manager edits** (`model_manager_processor.rs:192`) are the only path
   for a real user choice, and they also copy text the user never edited:
   - The TUI sends every field on each edit, including `baseInstructions`
     (`tui-chatwidget/src/chatwidget/model_manager_prompts.rs:140`), so
     changing only the context window writes the shown prompt back.
   - `read` fills `baseInstructions` from a prompt file when one exists
     (`model_manager_processor.rs:121`), so any later settings edit copies the
     file into the registry, where it outlives the file.
   - The update sets only `base_instructions` and leaves `model_messages`, so
     prompt edits on OpenAI models have no effect (`openai_models.rs:486`).

On this machine all 36 prompt texts in `~/.xedoc/models.json` (26 entries,
including templates) are exact copies of built-in text: `prompt.md` with or
without the footer, or a bundled `base_instructions`/`instructions_template`.

### Problems

- **Frozen prompts.** The registry is created once from built-in defaults
  (`registry.rs:121`) and migration only adds missing models (`registry.rs:265`).
  Changing `prompt.md` or `models.json` does not affect existing installs. On
  this machine, 684 KB of the 743 KB registry is prompt text, all of it copied
  built-in text.
- **Fake overrides.** Model-manager edits copy the shown prompt, or a prompt
  file's content, into the registry even when only a token setting changed
  (`model_manager_prompts.rs:140`, `model_manager_processor.rs:121`). Prompt edits on
  OpenAI models have no effect because the template wins
  (`openai_models.rs:486`).
- **Catalog text can replace or blank the prompt.** For providers without a
  registry entry, Xedoc-format catalogs (remote or config) replace the Xedoc
  prompt with their own text, which may be empty, and the model list and the
  session lookup return different prompts (`registry_manager.rs:154`, `:190`).
- **Wrong tool guidance.** `prompt.md:132` tells every non-OpenAI model to
  "use the `apply_patch` tool" with a legacy JSON call shape. Their fallback
  metadata sets `apply_patch_tool_type: None` (`model_info.rs:156`), so
  `core/src/tools/spec_plan.rs:518` registers no such tool. Editing works only
  through `exec_command` interception
  (`core/src/tools/handlers/unified_exec/exec_command.rs:313`), and
  `prompt.md:132` is the only place these models see the patch format. The
  written guide (`prompts/templates/apply_patch_tool_instructions.md`, exported
  as `APPLY_PATCH_TOOL_INSTRUCTIONS`) is never sent outside tests, and the
  freeform tool's own description is one sentence
  (`core-tool-specs/src/apply_patch_spec.rs:20`).
- **Thin plan tool.** The `update_plan` description is three lines
  (`core-tool-specs/src/plan_spec.rs:44`); the planning rules live only in
  `prompt.md`.
- **Rebrand artifacts.** `prompt.md:1` ("led by OpenAI"), `prompt.md:9`
  ("the old Xedoc language model built by OpenAI"), and
  `DEFAULT_PERSONALITY_HEADER` ("a coding agent based on GPT-5",
  `model_info.rs:20`).
- **Vendor artifacts.** `prompt.md:147` forbids OpenAI-internal citation
  markers that other models never produce.
- **Duplicated runtime facts.** `prompt.md:161` restates approval-mode
  behavior that the permissions developer message already carries.
- **Internal repetition in `prompt.md`.** Preambles and progress updates are
  covered twice (2.7 KB); planning appears in three places (3.7 KB); final
  answer formatting takes 5.9 KB.
- **Always-on niche guidance.** `gpt-5.5` carries ~7 KB of frontend design
  guidance on every request.
- **Inconsistent skills guidance.** `gpt-6-*` embeds the 4.5 KB skills how-to
  in the base prompt and sets `include_skills_usage_instructions: false`; other
  models get the same text from the skills fragment
  (`core-context/src/available_skills_instructions.rs:31`).
- **Broken personality on the newest models.** `gpt-6-*` and `gpt-5.6-*`
  templates contain no `{{ personality }}` placeholder, so the setting has no
  effect.
- **Dead and duplicate files.** Unreferenced: `core/gpt_5_1_prompt.md`,
  `core/gpt_5_2_prompt.md`, `core/gpt_5_codex_prompt.md`,
  `core/gpt-5.1-codex-max_prompt.md`, `core/gpt-5.2-codex_prompt.md`,
  `core/templates/model_instructions/gpt-5.2-codex_instructions_template.md`.
  Test-only: `core/prompt_with_apply_patch_instructions.md`.
  `protocol/src/prompts/base_instructions/default.md` is byte-identical to
  `models-manager/prompt.md` and backs `BaseInstructions::default()`, which
  only default and test paths use.

## Design

### Layer ownership

| Layer | Owns | Examples |
| --- | --- | --- |
| Base prompt | Stable behavior true in every session | working style, editing discipline, validation, communication, final answer format |
| Tool definitions | How and when to use each tool | patch grammar, plan tool semantics, shell usage |
| Developer fragments | Session-specific facts | sandbox, approval policy, collaboration mode, skills catalog and usage, AGENTS.md contents |

The base prompt must not name a tool as always available, restate a
permission or approval policy, or carry text for one provider, model family, or
task domain. It may mention a capability conditionally ("when a planning tool
is available").

### Built-in prompt files

`models-manager` remains the owner (it owns `BASE_INSTRUCTIONS` today and every
resolution path already passes through it). Files:

- `models-manager/prompts/base.md`: the shared prompt, containing one
  `{{ personality }}` placeholder.
- `models-manager/prompts/personality_friendly.md`
- `models-manager/prompts/personality_pragmatic.md`

`models-manager/prompt.md` is deleted. `BUILD.bazel` `compile_data` is updated
for the new `include_str!` paths.

Size caps, enforced at compile time with
`const _: () = assert!(BASE.len() <= N);`:

| File | Cap |
| --- | --- |
| `base.md` | 4,096 bytes (~1k tokens) |
| Each personality file | 512 bytes |

### Composition

The built-in composition is the floor for every provider and model, including
providers without registry entries and config `model_catalog` catalogs. A
single function in a new module `models-manager/src/instructions.rs` builds it:

```text
base.md with {{ personality }} placeholder
+ "\n\n# Model identity\nYou are running as {slug} from {provider_display_name}."
```

The identity footer is added whenever the provider and model are known. An
unknown slug without a provider gets the base prompt without the footer.

The function returns `ModelMessages { instructions_template, instructions_variables }`
with the shared personality variables, and sets `base_instructions` to the same
template rendered with an empty personality. The existing
`ModelInfo::get_model_instructions` then renders personality unchanged; no core
changes are needed.

Personality handling becomes uniform:

- Personality disabled, `none`, or default: the placeholder renders empty.
- `friendly` or `pragmatic`: the matching file is inserted.
- `strip_personality_section`, `DEFAULT_PERSONALITY_HEADER`,
  `LOCAL_*_TEMPLATE`, and `local_personality_messages_for_slug` are removed.
  Custom prompt text is used verbatim.

### Resolution

One function, `resolve_instructions`, decides the effective instructions. It
runs inside `RegistryModelsManager`, which wraps every provider's manager
(`core/src/thread_manager.rs:317`, `:374`), and replaces the separate prompt
assignments in the session lookup (`registry_manager.rs:154`), the model list
(`registry_manager.rs:45`, `:190`), the catalog fallback (`manager.rs:901`), the
`*` template (`registry.rs:173`), and the fallback builders
(`model_info.rs:149`, `model_info.rs:182`). The list and the session therefore
always agree.

Order:

1. Config `base_instructions` or `model_instructions_file` (unchanged, still
   handled in core).
2. Stored session metadata on resume (unchanged).
3. `~/.xedoc/prompts/<provider>/<model>.md`, then `~/.xedoc/prompts/<model>.md`:
   the model-specific user override (unchanged).
4. Provider-supplied catalog instructions, only when the new config key
   `model_remote_instructions = true` is set (default `false`) and the matching
   catalog entry has non-empty `base_instructions`. This covers OpenAI's Codex
   `/models`, Xedoc-format remote catalogs, and config `model_catalog`. The
   text is used verbatim, including OpenAI's "You are Codex" branding.
5. Built-in composition.

Rules:

- Neither the registry nor the bundled `models.json` is ever read for prompt
  text.
- A step-3 override replaces `base_instructions` and clears `instructions_*`,
  so no template can shadow it (today's shadowing is `openai_models.rs:486`).
  Step 4 takes the catalog's `base_instructions` and `instructions_*` together.
- `model_remote_instructions` is a `ConfigToml` field, forwarded through
  `ModelsManagerConfig`; `just write-config-schema` updates
  `core/config.schema.json`.

### Storage

Prompt text is never stored in either `models.json`.

- **Bundled catalog.** Every `models-manager/models.json` entry gets
  `base_instructions: ""` and `instructions_template`/`instructions_variables:
  null`. This is data cleanup, since the resolution never reads it. Approval,
  auto-review, and permission messages are untouched (all currently `null`).
  The wire type is unchanged.
- **Registry.** Defaults (`registry_defaults.rs`), migration, and
  `ensure_provider_config` (`model_manager_processor.rs:370`) write empty
  `base_instructions` and no `instructions_*`. `validate_managed_model`
  (`registry.rs:312`) accepts empty text.
- **Registry migration.** `MODEL_REGISTRY_SCHEMA_VERSION` is bumped. Migration
  deletes `base_instructions` and `model_messages.instructions_*` from every
  model and template. No fingerprint list is needed. It also sets
  `apply_patch_tool_type` and `include_skills_usage_instructions` to the new
  defaults (see below); neither is user-editable today.
- **Editor and API.** Model-manager prompt edits go to
  `~/.xedoc/prompts/<provider>/<model>.md`:
  - `read` returns the effective prompt: the prompt file when present,
    otherwise the step-4 or step-5 result.
  - `update` writes the file when the text differs from what the model would
    get without a file, and deletes the file when the text is empty or equal
    to that.
  - Edits that do not change the prompt text never write it anywhere,
    including edits that only change the context window or compaction limit.
  - The wire shape of `ManagedModelSettings.baseInstructions`
    (`app-server-protocol/src/protocol/v2/model.rs:516`) and
    `ModelSettings.base_instructions` is unchanged.

### Tool-owned guidance

- **Editing.** Every provider and model gets the `apply_patch` tool.
  - `ApplyPatchToolType` has only `Freeform` (`protocol/src/openai_models.rs:295`),
    and no `Function` variant is needed. The Anthropic adapter
    (`provider-anthropic/src/request.rs:90`), which DeepSeek also uses
    (`model-provider/src/deepseek_models_endpoint.rs:14`), and the Gemini
    adapter (`provider-gemini/src/request.rs:331`) already turn the freeform
    `apply_patch` tool into a function tool with one `input` field. They
    forward only the tool's `description` and drop the Lark grammar.
  - The fallback builder (`model_info.rs:156`) and registry defaults set
    `apply_patch_tool_type = Freeform`; migration sets it on registry entries
    that have `null`.
  - Those two adapters append the written patch format to the `apply_patch`
    description. OpenAI models keep only the Lark grammar
    (`core-tool-specs/src/apply_patch_spec.rs:5`), so they never get the format
    twice. The text is today's `prompts/templates/apply_patch_tool_instructions.md`
    (3,084 bytes), moved into `xedoc-protocol`, which both adapters already
    depend on, with a hard cap of 3,584 bytes. Adapters do not go through
    `ContextualUserFragment`, so the cap is the only bound.
  - `exec_command` interception (`core/src/tools/handlers/unified_exec/exec_command.rs:313`)
    stays as the fallback it is today.
  - The base prompt only says to edit files with the patch tool when one is
    available.
- **Skills.** Every model uses `include_skills_usage_instructions: true`, so
  the skills how-to arrives with the skills catalog and only when skills exist.
- **Planning.** Today the `update_plan` description is three lines
  (`core-tool-specs/src/plan_spec.rs:44`). The rules for what a step is, one
  step in progress, and when to update move into that description, drawn from
  `prompt.md:54` and `prompt.md:267` without the example lists, with a hard cap
  of 1,024 bytes. The base prompt keeps only when to plan and when to skip,
  with "when one is available", because `update_plan` errors in Plan mode
  (`collaboration-mode-templates/templates/plan.md:15`).

### Removed without replacement

- The approval-mode testing rules (the permissions fragment owns them).
- The OpenAI citation-marker rule.
- Preamble and plan example lists.

### Built-in skills

- **Frontend design.** The `gpt-5.5` "Build with empathy" and "Design
  instructions" sections (~7 KB in `models-manager/models.json`) move into a
  new built-in skill at `xedoc-rs/skills/src/assets/samples/frontend-design/SKILL.md`
  instead of being dropped.
  - Its description says clearly that it applies to frontend and UI work.
  - It keeps implicit invocation on and declares no dependencies, so the model
    is offered it without being asked.
  - Built-in skills are on by default in every session
    (`core-skills/src/service.rs:306`) unless `skills.bundled.enabled = false`.
  - No Bazel change is needed (`skills/BUILD.bazel` globs the directory), and
    existing installs pick it up through the fingerprint check in
    `skills/src/lib.rs:53`.
- **Remove `openai-docs`.** Delete `xedoc-rs/skills/src/assets/samples/openai-docs/`.
  No shipped code depends on it; the only other mention is fixture data in
  `core/tests/common/context_snapshot.rs:707`. Installs drop it automatically
  because the installer rewrites `.system` when the shipped skills change
  (`skills/src/lib.rs:59`).
- **Keep `review-agent`.** Detached `review/start` builds its prompt around
  that skill's path (`app-server/src/request_processors/turn_processor.rs:1116`,
  `app-server/README.md:1036`). It costs no context because
  `allow_implicit_invocation: false` keeps it out of the skill list.

## Draft base prompt

This is the proposed `base.md` (3.4 KB, ~840 tokens, under the 4,096-byte cap).

```markdown
You are Xedoc, a coding agent working in the user's terminal and workspace. You collaborate with the user to complete software tasks precisely, safely, and efficiently.

{{ personality }}

# Instructions

Follow system and developer messages first, then the user, then AGENTS.md files. Developer messages describe this session's sandbox, approval policy, collaboration mode, skills, and tools; treat them as authoritative.

AGENTS.md files hold repository guidance. Each applies to the directory tree that contains it, and deeper files override shallower ones. Before editing a file, follow every AGENTS.md whose scope includes it. Files from the working directory up to the repository root are already in context; look for others when you work outside that path.

# Doing the work

- Keep going until the task is resolved or you need something only the user can provide. Do not stop at a plan or analysis when the user asked for a change.
- Ground claims in the workspace. Read the relevant code, configuration, and tests before changing them; do not guess about behavior you can check.
- When a wrong guess would be costly, ask one concise question. Otherwise state your assumption and proceed.
- Fix the root cause with the smallest complete change. Match the surrounding style. Do not rename, reformat, or refactor unrelated code.
- Do not add license headers, comments that restate the code, or one-letter names unless asked.
- Edit files with the patch tool when one is available. Do not reread a file only to confirm a successful edit.
- Search with `rg` and `rg --files` when available.
- Do not commit, branch, or push unless asked.
- Preserve changes you did not make. Do not run destructive commands such as `git reset --hard`, `git checkout --`, or `rm -rf` unless the user asked for that exact action.

# Validation

- Start with the narrowest check that covers your change, such as one test, one type check, or one build target. Widen only as confidence requires.
- Follow repository instructions about which checks to run and whether to add tests.
- Do not fix unrelated failures; mention them.

# Planning

Use the planning tool, when one is available, for work with several distinct steps. Skip it for simple tasks.

# Communication

These rules cover your messages, plans, and final answers. Files you write follow repository conventions.

- Write plainly: common words, active voice, and short sentences that stay precise. Cut words that add nothing.
- Name exact files, commands, and values instead of describing them.
- Say what you verified, what you inferred, and what you are unsure of.
- Leave out filler, hype, and narration of routine work. Before a group of related tool calls, send one sentence on what you are about to do; on long tasks, send a brief update after each meaningful step.

# Final answer

- Lead with the outcome. For changes, state what changed and why, how you validated it, and any remaining risk.
- Keep it short: a few sentences or up to six bullets for most tasks. Use headers only when the answer has distinct parts.
- Reference files as workspace-relative paths with an optional line, for example `src/app.ts:42`. Do not use URIs or line ranges.
- Wrap commands, paths, environment variables, and identifiers in backticks. Use `-` bullets without nesting.
- The user sees the workspace; do not paste files you wrote.
```

Draft personality files:

- `personality_friendly.md`: "Be a warm, encouraging teammate. Explain your
  reasoning in plain language and acknowledge progress, without padding."
- `personality_pragmatic.md`: "Be a terse, pragmatic senior engineer. Focus on
  results and trade-offs; skip pleasantries."

Writing style lives in `base.md`, outside the `{{ personality }}` placeholder,
so it applies when personality is `none` or default. Personality files only
adjust tone and must not contradict the base style rules.

## Compatibility

- **Resumed sessions** keep their stored instructions
  (`core/src/session/mod.rs:746`). A model switch re-renders from the new model,
  as it does today (`core/src/session/mod.rs:1353`).
- **Config overrides and `~/.xedoc/prompts`** are unchanged and remain the way
  to customize one model.
- **Registry prompt text** is deleted by migration. On this machine every
  stored prompt is a built-in copy, so nothing is lost; hand-edited registry
  prompts on other installs are dropped by decision.
- **OpenAI models** switch from OpenAI-authored prompts to the shared prompt.
  `model_remote_instructions = true` restores OpenAI's current prompts from
  `/models`, which the registry hides today.
- **Model-manager edits** now write prompt files instead of the registry. The
  app-server wire types are unchanged.
- **New config key** `model_remote_instructions` (default `false`).
- **Prompt cache.** The first request after upgrade misses the cache once per
  model; afterwards the prefix is stable.

## Validation

Per the repository test policy, validation is behavioral and narrow:

- Render the effective instructions for one model per provider, with each
  personality and with `model_remote_instructions` on and off, and compare
  them against the caps.
- Run the migration on a copy of the current `~/.xedoc/models.json` and confirm
  every prompt field is gone and `apply_patch_tool_type` is `freeform`.
- In the model manager, change only a context window and confirm no prompt is
  written; save a custom prompt and confirm the file appears; reset it to the
  built-in text and confirm the file is deleted.
- Smoke-test one session each on Anthropic, Google, DeepSeek, and OpenAI with
  one fixed task: edit a file with `apply_patch`, run one check, and give a
  final answer.
- Run a scoped Bazel build of the changed crates and their direct dependents,
  plus `just fix -p <crate>` and `just fmt`.

## Delivery stages

The registry change must land with or before the new prompt content;
otherwise existing installs keep their frozen copies.

1. **Editing tool.** Set `apply_patch_tool_type = Freeform` in the fallback
   builder and registry defaults, migrate `null` registry entries, move the
   patch-format text into `xedoc-protocol` with its cap, and append it in the
   Anthropic and Gemini adapters. The tool and its format land together, so no
   model is ever without the patch format. `fix` patch bump. About 150 lines.
2. **Resolution and storage.** Add `instructions.rs` with the current
   `prompt.md` text as `base.md`, implement `resolve_instructions` in
   `RegistryModelsManager`, add `model_remote_instructions`, stop storing
   prompt text (defaults, `ensure_provider_config`, migration), route editor
   prompt edits to prompt files, strip `models.json` prompt text, and add the
   `frontend-design` skill in the same change that removes the `gpt-5.5` text.
   OpenAI models switch to the shared prompt. `feat` minor bump. About 350–450
   lines excluding the mechanical `models.json` rewrite and the skill text; if
   review needs it, split the editor change into its own step right after.
3. **New prompt content.** Replace `base.md` with the draft, add the
   personality files and caps, expand the `update_plan` description with its
   cap, set `include_skills_usage_instructions` for all models, and remove the
   personality-stripping code. The plan-tool change lands here so planning
   guidance is never missing. `feat` minor bump. About 200 lines.
4. **Cleanup.** Delete the unused `core/*_prompt.md` files, the unused
   template, `core/prompt_with_apply_patch_instructions.md`, the `xedoc-prompts`
   patch-format copy and its `APPLY_PATCH_TOOL_INSTRUCTIONS` export
   (`prompts/src/lib.rs:8`), `protocol/src/prompts/base_instructions/default.md`
   (with `BaseInstructions::default()` returning empty text), and the
   `openai-docs` built-in skill. Deleting `openai-docs` changes the shipped
   skills, so it is a `refactor` patch bump; the rest is deletion only.

## Open questions

None.

### Decisions

- **OpenAI-authored prompts** are dropped for OpenAI models. Model-specific
  overrides stay possible through `~/.xedoc/prompts/` and the opt-in
  `model_remote_instructions`.
- **Provider-supplied instructions** are optional and off by default; Xedoc
  relies on its built-in instructions.
- **Built-in floor.** Every provider and model, including providers without
  registry entries and config `model_catalog` catalogs, gets the built-in
  base prompt. Catalog text never replaces it unless
  `model_remote_instructions` is on.
- **Registry** never stores or supplies prompt text.
- **Registry prompt migration.** Migration deletes all registry prompt text.
  Hand-edited registry prompts on other installs are not moved to prompt
  files, so no fingerprint list of legacy built-in texts is needed.
- **Editing tool.** Every model gets `apply_patch`; the written patch format
  goes into its description only where an adapter drops the grammar.
- **Frontend design guidance** moves into a built-in skill. `openai-docs` is
  removed; `review-agent` stays.
