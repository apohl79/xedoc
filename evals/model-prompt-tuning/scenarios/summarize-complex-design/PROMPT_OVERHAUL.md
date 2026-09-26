# Shared model-prompt redesign

## Decision

The agent runtime needs one compact base prompt for every provider and model.
It must describe stable working behavior only: how to inspect a task, make
changes, validate them, and report the result. It must not contain rules that
depend on a particular model, provider, tool set, sandbox, approval mode, or
collaboration setting.

The current system has several large overlapping prompts. Some are bundled
with named models, some are copied into the user's model registry, and some
come from a provider catalog. The same guidance is often repeated in base
instructions, developer messages, and tool descriptions. This creates three
problems: prompt changes do not reach installations that already copied an old
prompt; a model can receive guidance for a tool it does not have; and the
effective instructions differ between the model picker and a running session.

## Ownership boundary

The runtime owns the default base prompt. A provider's catalog instructions are
not used by default because they are provider-specific and can change without
the runtime's review. They may be enabled deliberately through a configuration
switch. A user may also supply a model-specific prompt file or an explicit
configuration override. Those choices replace the built-in prompt; they are
not merged with it.

The registry retains model capabilities and user-selected settings, but it no
longer stores any base-prompt text. During migration, old copied prompt text is
removed. This lets an updated runtime change its built-in prompt for existing
installations without changing the user's chosen model settings.

## Composition

One instruction resolver determines the effective instructions for every
model-list and session-start path. Its precedence is:

1. An explicit configuration override or instruction file.
2. A provider-specific user prompt file, then a provider-agnostic prompt file.
3. Provider catalog instructions, only when the opt-in switch is enabled.
4. The built-in base prompt plus an optional personality fragment and a model
   identity footer.

The built-in base prompt has a fixed size limit. Personality fragments have
their own smaller limits. The identity footer tells the model which provider
and model it is running as, but the same core instructions precede it for every
model.

## Developer messages and tools

Conditional facts belong where the runtime knows whether they are true.
Developer-message fragments carry permission policy, sandbox behavior,
collaboration mode, available skills, and other session facts. The base prompt
does not restate them.

Tool definitions carry tool-specific semantics. The patch tool includes the
actual patch grammar and guidance in its definition, so a model sees it only
when that tool is registered. The plan tool describes when to create, update,
and complete a task list. Provider adapters add only their required patch-tool
translation details. This removes provider-specific syntax and planning
boilerplate from the shared base prompt.

## Compatibility and validation

The resolver is used by both the model picker and session construction, so
they cannot disagree about a model's effective instructions. The app-server
editor reads and writes only user prompt files; editing another model setting
does not accidentally copy prompt text into the registry.

Validation has two layers. Focused integration tests cover resolution
precedence, the registry migration, and provider-specific patch tool
registration. Real isolated end-to-end runs enable complete sampling-request
logging and inspect the exact backend request. The inspection must verify the
base instructions, developer fragments, available tools, provider translation,
personality, model identity, and each override path. Behavioral comparisons
also measure whether a shorter prompt changes task completion, tool use, token
cost, or response quality.
