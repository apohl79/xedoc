# Session script interface

This document defines a persistent script interface for one loaded Xedoc
session. It lets multiple scripts observe a session, receive model output and
interactive prompts, answer `request_user_input`, submit ordinary user input,
and read current session and turn state.

The interface extends app-server v2 instead of the one-shot
`xedoc.script/v1` subprocess protocol. Session scripts need asynchronous,
bidirectional communication for the lifetime of a loaded thread; app-server
already owns that transport, thread subscriptions, turn input, event ordering,
reconnection, and public item types.

## Goals

- Register multiple independent script connections for one loaded root thread.
- Give every registered script an atomic initial snapshot and a resync method.
- Stream assistant output during a turn and expose the authoritative completed
  response and completed turn.
- Let scripts observe input prompts without making every observer an eligible
  responder.
- Let at most one script answer `request_user_input` for a session.
- Let explicitly permitted scripts start a turn or steer the active regular
  turn with ordinary user input.
- Expose the session title, project name, active cwd, thread state, and active
  turn.
- Bound queues and payloads so a stalled script cannot block the session or
  accumulate unbounded history.

## Non-goals

- Replacing app-server v2 with a second general-purpose RPC protocol.
- Turning the existing one-shot model-router process into a persistent daemon.
- Giving scripts arbitrary app-server mutation rights.
- Allowing scripts to answer command, file-change, permission, MCP, or router
  approvals in the first version.
- Replaying the complete transcript when a script connects.
- Attaching one script connection to multiple threads. A script that needs
  multiple sessions opens one connection per session.

## Existing primitives

The design reuses these app-server v2 contracts:

| Requirement | Existing contract |
| --- | --- |
| Output while a model response is streaming | `item/agentMessage/delta` |
| Authoritative completed model message | `item/completed` containing an `agentMessage` item |
| End of turn | `turn/completed`, including the complete `Turn` |
| New user turn | `turn/start` |
| User input during an active regular turn | `turn/steer` with `expectedTurnId` |
| `request_user_input` prompt and answer | `item/tool/requestUserInput` server request and response types |
| Router approval prompt | `item/extensionInteraction/request` |
| Session title changes | `thread/name/updated` |
| Active cwd and other sticky settings | `thread/settings/updated` |
| Thread lifecycle state | `thread/status/changed` |

An ordinary app-server connection currently receives thread notifications
after `thread/start`, `thread/resume`, or `thread/fork`. Interactive server
requests are sent to all subscribed connections and the first response wins.
That behavior is unsuitable for automation: an observing script could race the
interactive client. Session-script registration therefore adds roles and
prompt ownership rather than subscribing scripts as indistinguishable ordinary
clients.

## Connection model

A session script uses a normal initialized app-server v2 connection:

1. Open an authenticated app-server transport.
2. Send `initialize` and `initialized` as usual.
3. Call `script/register` with one loaded root `threadId`.
4. Read the returned snapshot before processing later notifications.
5. Use only the capabilities granted in the registration response.
6. Disconnect, or call `script/unregister`, to remove the registration.

`script/register` atomically subscribes the connection to the thread and
captures its initial snapshot. The response is ordered before subsequent
thread notifications on that connection. The script does not need to call
`thread/resume`, and registration never loads an unloaded thread.

One connection may have at most one active registration. Multiple connections
may register with the same thread. Registration is scoped to the root thread
only; subagent activity remains visible through the root thread's existing
items and collaboration notifications but does not silently subscribe the
script to child threads.

The registration is removed automatically when the transport closes, the
thread closes, or the app-server shuts down. A session-script subscription
counts as a thread subscriber for the existing delayed-unload lifecycle.

## Launch and authorization

The initial implementation should support host-managed local scripts. Each
configured entry is an argv vector, never a shell command:

```toml
[[session_scripts]]
id = "desktop-notifier"
command = ["~/.xedoc/scripts/desktop-notifier"]
subscriptions = [
  "modelResponseCompleted",
  "turnCompleted",
  "prompt.requestUserInput",
  "prompt.extensionInteraction",
]
capabilities = ["userInput.send"]
response_timeout_ms = 10000
```

For every loaded root thread, Xedoc starts one process per enabled entry.
Each process gets a dedicated app-server JSON-RPC connection over stdin/stdout;
stderr remains bounded diagnostic output. Xedoc supplies the target thread and
configured script ID through the child environment, then validates that
`script/register` matches that pre-authorized scope. This also works when the
TUI uses an embedded app-server that has no externally reachable socket.

Multiple entries produce independent processes and registrations. A process
failure removes only its own registration and never stops the thread or another
script.

Extensions need an interactive first-run lifecycle. On initial launch, an
extension may request user input through the normal Xedoc prompt surface to
collect setup data such as backend configuration or credentials. Secret values
follow the existing secret-input handling rules and are delivered only to the
requesting extension.

An extension may declare session-scoped slash commands for activation and
deactivation, such as `/signal enable` and `/signal disable` for a Signal
bridge. Xedoc rejects malformed declarations and command-name collisions before
activation.

Launching or enabling an extension requires an explicit Xedoc approval flow,
with review and “approve all” choices analogous to hook approval. Only
approved extensions may register or activate. Plugins may ship extensions in
their manifests; plugin trust supplies candidate metadata, while per-session
approval controls whether each extension can run.

### Reuse the one-shot interaction protocol

`xedoc.script/v1` already has the right host-to-script envelope
(`ScriptRequest`/`ScriptResponse`), the `Interaction` result, continuations,
state revisions, and the declarative `menu`, `form`, `confirmation`, and
`notice` surfaces. The existing TUI `ScriptedInteractionView` already renders
those surfaces. Session extensions must reuse that path; they do not get a
second UI protocol or a separate extension-specific renderer.

Refactor the invocation machinery currently named `ModelRouterScriptHost` into
a generic bounded one-shot script invoker. The model-router adapter continues
to call it for `routing.decide`, `settings.open`, and
`interaction.respond`, retaining all route-specific validation. The extension
adapter calls the same invoker for these new `xedoc.script/v1` methods:

| Method | Purpose | Expected result |
| --- | --- | --- |
| `extension.setup.open` | Open first-run or reconfiguration UI after approval. | `interaction` |
| `extension.command.invoke` | Handle an approved extension slash command. | `interaction` or a bounded success/error result |
| `interaction.respond` | Submit an accepted, cancelled, or dismissed surface. | Next `interaction` or terminal success/error |

`ScriptRequest.extension` must gain a session-extension value while preserving
the existing model-router value and wire names. The host supplies the
extension ID, thread ID, session snapshot, and command arguments as bounded
context; it retains authority for approval, registration, capability grants,
and command dispatch. A script can propose only a declarative UI and opaque
actions, never arbitrary terminal UI behavior or host mutations.

The existing `Text` form field is the entry field for setup values. Extend it
with `sensitive: true` when a value must be masked and excluded from transcript,
diagnostic, event, and notification payloads. Do not add a separate credential
dialog or a second form system. Menus are the default navigation surface;
forms are reserved for the small number of values that cannot be selected.

### Signal bridge example

The plugin manifest adds an `extensions` declaration alongside existing plugin
resources. It is discoverable at installation time but does not start code:

```json
{
  "name": "signal-bridge",
  "extensions": [
    {
      "id": "signal",
      "entrypoint": "./extensions/signal-bridge",
      "commands": [
        {
          "name": "signal",
          "description": "Configure or control the Signal bridge"
        }
      ],
      "requestedCapabilities": [
        "session.observe",
        "userInput.send",
        "prompt.requestUserInput.respond"
      ]
    }
  ]
}
```

`/signal` is registered with the existing slash-command completion and
dispatch path. It invokes `extension.command.invoke` with the remaining words
as arguments rather than creating separate `/signal-enable` and
`/signal-disable` commands. The first response is a menu:

```json
{
  "kind": "interaction",
  "interaction": {
    "id": "signal-menu",
    "continuation": "opaque-state",
    "stateRevision": "3",
    "surface": {
      "type": "menu",
      "title": "Signal bridge",
      "items": [
        {
          "id": "connect",
          "label": "Connect Signal",
          "description": "Set up this session's Signal account",
          "action": { "id": "connect", "keyBindings": ["enter"] }
        },
        {
          "id": "disable",
          "label": "Disable for this session",
          "description": "Keep the extension installed without observing this session",
          "action": { "id": "disable" }
        }
      ]
    }
  }
}
```

Selecting **Connect Signal** follows the existing
`interaction.respond` continuation and returns a form. The host renders its
standard form view:

```json
{
  "kind": "interaction",
  "interaction": {
    "id": "signal-connect",
    "continuation": "opaque-state",
    "stateRevision": "4",
    "surface": {
      "type": "form",
      "id": "connect",
      "title": "Connect Signal",
      "fields": [
        {
          "type": "select",
          "id": "transport",
          "label": "Connection method",
          "options": [
            { "id": "desktop", "label": "Signal Desktop" },
            { "id": "relay", "label": "Relay service" }
          ]
        },
        {
          "type": "text",
          "id": "relay-url",
          "label": "Relay URL",
          "maxBytes": 2048
        },
        {
          "type": "text",
          "id": "access-token",
          "label": "Access token",
          "sensitive": true,
          "maxBytes": 4096
        }
      ],
      "submit": { "id": "save", "label": "Connect" },
      "cancel": { "id": "cancel", "label": "Cancel" }
    }
  }
}
```

The extension receives only the accepted response's opaque action and field
values. It owns its configuration persistence; Xedoc does not store sensitive
values in the thread, rollout, or extension-registration state. A terminal
`notice` can report success or a safe remediation message.

### Approval and activation flow

Approval happens before the host invokes extension setup or enables its session
capabilities. The host builds a standard confirmation surface from signed or
installed manifest metadata, not from an unapproved process:

```text
Allow “Signal bridge” in this session?

Plugin: signal-bridge 1.4.0
Command: /signal
Requested access:
  - Observe model responses and session state
  - Send ordinary user messages
  - Answer request-user-input prompts

[Approve for this session] [Approve always] [Deny]
```

`Approve for this session` permits setup and later command activation only for
the current root thread. `Approve always` persists an extension grant across
sessions, keyed by the plugin identity and the digest of its declared
capabilities and commands. A plugin update, capability or command change, or
different extension requires review again. A denial never launches the
extension, registers its slash command as executable, or creates a
session-script connection.

After approval, the host invokes `extension.setup.open` only when the
extension reports setup is required. Successful setup enables the declared
slash command and permits the extension to register its persistent
app-server connection. `/signal disable` revokes that session registration and
stops future activation without changing the plugin installation or another
session's approval.

Externally connected session scripts can be added later with a one-use,
thread-bound registration token. A client-supplied script ID alone is not
authorization to observe a session or acquire capabilities.

## Registration API

The API is experimental in its first version.

### `script/register`

```json
{
  "method": "script/register",
  "id": 17,
  "params": {
    "threadId": "0199...",
    "script": {
      "id": "desktop-notifier",
      "name": "Desktop notifier",
      "version": "1.2.0"
    },
    "subscriptions": {
      "modelResponseDeltas": true,
      "modelResponseCompleted": true,
      "turnCompleted": true,
      "prompts": [
        "requestUserInput",
        "extensionInteraction"
      ],
      "sessionUpdates": true
    },
    "requestedCapabilities": [
      "userInput.send"
    ]
  }
}
```

All optional subscription booleans default to `false`. Prompt kinds are an
explicit enum rather than method-name prefixes. Unknown values are rejected so
a script cannot believe it is observing a prompt class that the host ignored.

The response contains the effective registration and one atomic snapshot:

```json
{
  "id": 17,
  "result": {
    "registrationId": "0199...",
    "grantedCapabilities": [
      "userInput.send"
    ],
    "snapshot": {
      "revision": 42,
      "session": {
        "sessionId": "0199...",
        "threadId": "0199...",
        "title": "Design session scripts",
        "projectName": "xedoc",
        "projectRoot": "/workspace/xedoc",
        "cwd": "/workspace/xedoc/xedoc-rs"
      },
      "thread": {
        "status": {
          "type": "active",
          "activeFlags": []
        },
        "canAcceptDirectInput": true
      },
      "turn": {
        "id": "0199...",
        "status": "inProgress",
        "startedAt": 1789560000
      },
      "pendingPrompts": []
    }
  }
}
```

`title`, `projectName`, `projectRoot`, and `turn` are nullable. `cwd` is the
effective active thread cwd, not the app-server process cwd.

`projectRoot` is resolved with the same rule used by the TUI: prefer the Git
repository root, then the nearest project configuration root, then no root.
`projectName` is the display name of `projectRoot`, falling back to the cwd
name. This logic should move to a small shared utility rather than be
reimplemented in app-server.

### `script/unregister`

```json
{
  "method": "script/unregister",
  "id": 18,
  "params": {
    "registrationId": "0199..."
  }
}
```

The operation is idempotent for the owning connection. A connection cannot
unregister another connection's registration.

### `script/read`

```json
{
  "method": "script/read",
  "id": 19,
  "params": {
    "registrationId": "0199..."
  }
}
```

The response contains a fresh `snapshot` with a monotonically increasing
connection-local `revision`. Scripts use it after reconnecting, detecting an
event gap, or receiving `script/resyncRequired`.

The snapshot includes only the active turn, not historical turns or transcript
items. Scripts that have broader app-server authorization may use the existing
history APIs separately, but session-script registration does not grant them.

`pendingPrompts` contains the same bounded prompt projections used by
`script/promptOpened`. If the registration acquires the exclusive
request-user-input responder capability while such a prompt is pending, its
snapshot contains a newly issued response lease.

## Capabilities and method restrictions

Registration changes the connection into the restricted `sessionScript` role.
The role may call:

- `script/read`
- `script/unregister`
- `turn/start` and `turn/steer` when `userInput.send` is granted
- `script/respond` when the connection owns the matching prompt lease

All other mutating app-server methods are rejected for that connection. Read
access is limited to the registration snapshot unless a future capability
explicitly widens it.

Initial capability names:

| Capability | Effect |
| --- | --- |
| `userInput.send` | May call `turn/start` and `turn/steer` for the registered thread. |
| `prompt.requestUserInput.respond` | May own and answer one `requestUserInput` prompt at a time. |

The host computes `grantedCapabilities` from the requested capabilities and
host policy. A script cannot grant itself a capability by declaring it.
Host-managed scripts should receive grants from their configured script entry;
externally connected scripts use the app-server transport's existing
authentication boundary plus an explicit host policy.

Every permitted turn mutation is pinned to the registered `threadId`.
Cross-thread input is rejected even if the request payload names another
thread.

## Model-response delivery

Registered scripts receive projections of existing ordered app-server events.
The payload types remain the app-server v2 types so scripts do not need a
second model of a turn or item.

### During a response

When `modelResponseDeltas` is enabled:

```json
{
  "method": "item/agentMessage/delta",
  "params": {
    "threadId": "0199...",
    "turnId": "0199...",
    "itemId": "msg_123",
    "delta": "partial text"
  }
}
```

Deltas are best-effort progress. A script must not treat concatenated deltas as
the authoritative final message.

### Completed response item

When `modelResponseCompleted` is enabled, the script receives
`item/completed` only for `agentMessage` items:

```json
{
  "method": "item/completed",
  "params": {
    "threadId": "0199...",
    "turnId": "0199...",
    "item": {
      "type": "agentMessage",
      "id": "msg_123",
      "text": "authoritative complete text",
      "phase": "finalAnswer"
    },
    "completedAtMs": 1789560000123
  }
}
```

### End of turn

When `turnCompleted` is enabled, the script receives the existing
`turn/completed` notification. Its `Turn` is authoritative for terminal status,
errors, timestamps, duration, and all retained turn items.

A script that only needs one callback per turn can subscribe to
`turnCompleted` and omit both higher-volume model-response subscriptions.

## Session and turn state

The registration snapshot is the source of truth at connect time. Later state
changes use existing notifications:

- `turn/started`
- `turn/completed`
- `thread/status/changed`
- `thread/name/updated`
- `thread/settings/updated`
- `thread/closed`

When `sessionUpdates` is enabled, title or cwd changes additionally emit a
small derived notification so scripts do not need to reproduce project-name
resolution:

```json
{
  "method": "script/sessionUpdated",
  "params": {
    "registrationId": "0199...",
    "revision": 43,
    "session": {
      "sessionId": "0199...",
      "threadId": "0199...",
      "title": "Renamed session",
      "projectName": "xedoc",
      "projectRoot": "/workspace/xedoc",
      "cwd": "/workspace/xedoc/xedoc-rs"
    }
  }
}
```

The notification contains the complete replacement `session` object. Scripts
must replace their cached value instead of patching fields.

## Sending user input

No script-specific input method is needed. A script with `userInput.send` uses
the existing APIs:

- If the snapshot is idle, call `turn/start`.
- If a regular turn is active, call `turn/steer` with its
  `expectedTurnId`.

The normal turn-state preconditions remain authoritative. If the state changes
between the script's read and request, app-server returns the existing conflict
error; the script reads `script/read` and decides whether to retry. The
host does not silently reinterpret a stale `turn/start` as steering or a stale
`turn/steer` as a new turn.

`clientUserMessageId` should be populated by scripts so retries can be
correlated with the resulting `userMessage` item.

## Prompt observation

Scripts observe prompts through a notification, never by receiving an
actionable copy of every app-server server request:

```json
{
  "method": "script/promptOpened",
  "params": {
    "registrationId": "0199...",
    "promptId": "0199...",
    "kind": "extensionInteraction",
    "threadId": "0199...",
    "turnId": "0199...",
    "itemId": "call_123",
    "canRespond": false,
    "request": {
      "method": "item/extensionInteraction/request",
      "params": {
        "extensionId": "model-router",
        "interactionId": "route-approval",
        "surface": {
          "type": "confirmation",
          "title": "Use a different model?"
        }
      }
    }
  }
}
```

Initial prompt kinds are:

- `requestUserInput`
- `extensionInteraction`
- `commandExecutionApproval`
- `fileChangeApproval`
- `permissionsApproval`
- `mcpElicitation`

The first version supports response ownership only for `requestUserInput`.
Other kinds are observable so scripts can notify, log, or coordinate, while
the ordinary interactive client remains their responder.

Secret request-user-input questions retain `isSecret: true`, but their question
and option labels are still delivered to a script that explicitly subscribed
to the prompt. Host policy should therefore treat prompt observation as
sensitive read access.

Every terminal path emits:

```json
{
  "method": "script/promptClosed",
  "params": {
    "registrationId": "0199...",
    "promptId": "0199...",
    "reason": "answered"
  }
}
```

`reason` is one of `answered`, `cancelled`, `expired`, `turnEnded`,
`responderDisconnected`, or `superseded`.

## Answering `request_user_input`

`prompt.requestUserInput.respond` is exclusive per thread. The first
registration granted that capability owns it until unregister or disconnect.
A second registration requesting it receives a capability-conflict error; it
may still register without that capability.

When a request opens:

1. Every subscribed observer receives `script/promptOpened`.
2. If a responder is registered, its notification has `canRespond: true` and a
   fresh opaque `responseLease`.
3. While that lease is active, ordinary clients do not receive an actionable
   copy of the server request. They still see the thread's
   `waitingOnUserInput` state.
4. The responder calls `script/respond`.
5. Xedoc validates the registration, prompt, lease, question IDs, and answer
   shape, then resolves the existing core `request_user_input` holder.
6. Every observer and ordinary client sees the prompt close.

Example response:

```json
{
  "method": "script/respond",
  "id": 20,
  "params": {
    "registrationId": "0199...",
    "promptId": "0199...",
    "responseLease": "opaque-single-use-token",
    "response": {
      "kind": "requestUserInput",
      "answers": {
        "scope": {
          "answers": [
            "Current project"
          ]
        }
      }
    }
  }
}
```

The lease is single-use and bound to the connection, registration, prompt, and
prompt revision. Late or duplicate responses are rejected without changing
turn state.

If the responder disconnects, unregisters, or misses a host-configured response
deadline, Xedoc revokes the lease and sends the original actionable server
request to the ordinary subscribed clients. The core request remains pending;
no synthetic empty answer is submitted. The tool's existing
`autoResolutionMs`, when present, remains the outer deadline and wins if it
expires first.

This exclusive lease deliberately replaces the existing "broadcast one server
request and accept the first response" behavior for registered session
scripts. Ordinary non-script app-server clients retain their current behavior
until prompt routing is generalized separately.

## Backpressure and recovery

Each registration has its own bounded outbound queue and byte budget.

- Terminal events, completed model messages, prompt lifecycle, and session
  updates are reliable while the connection remains healthy.
- Model-response deltas are coalescible and may be dropped when their queue
  budget is exhausted.
- Dropping any delta emits one `script/resyncRequired` notification after the
  queue recovers.
- A script can recover current state with `script/read`; completed
  response text remains available from the later `item/completed` or
  `turn/completed` event.
- A connection that cannot accept reliable events is disconnected. It never
  blocks the thread listener or other clients.

Every notification carries the registration ID, either directly for new
script-specific notifications or implicitly through the connection's sole
registration. New script-specific state notifications also carry the
connection-local monotonic revision.

No automatic historical replay occurs after reconnect. A new registration
receives the current snapshot and future events.

## Security and data boundaries

- Registration requires an already authenticated app-server transport.
- The target must be a loaded root thread visible to that app-server instance.
- Capabilities are host-granted and checked on every mutation.
- A session-script connection is pinned to one thread.
- Prompt observation is a sensitive capability because prompts may contain
  commands, paths, approval reasons, or secret-input labels.
- Secret answers are delivered only to the selected responder and are never
  echoed in prompt-close notifications or diagnostics.
- Logs record registration IDs, script IDs, event kinds, sizes, and outcomes,
  but not prompt bodies, answers, user input, or model response text.
- Existing app-server payload and transport limits remain in force; the
  registration snapshot and prompt projections receive explicit per-field and
  total-size caps.

## Compatibility

- Existing app-server clients do not call `script/register` and keep their
  current behavior.
- Existing `scripts/xedoc-session` can continue to use `thread/read`,
  `turn/start`, and `turn/steer`. It can later adopt registration for streaming
  and prompt handling.
- Existing `xedoc.script/v1` model-router invocations remain one-shot and
  unchanged.
- Router approval continues to use the generic extension-interaction lifecycle;
  session scripts observe its app-server projection rather than talking to the
  router subprocess directly.

## Implementation boundaries

Keep the implementation out of `xedoc-core` except for reusing existing core
operations and events:

- `app-server-protocol`: add experimental request, response, notification, and
  capability types.
- `app-server`: own registration state, capability checks, thread binding,
  snapshots, prompt leases, and event projection.
- `app-server-jsonrpc`: route server requests to selected connections instead
  of broadcasting actionable requests to script observers.
- `config` and `core-config`: define and resolve the host-managed script list;
  regenerate the config schema with the normal repository workflow.
- A small shared project-identity utility: resolve project root and display
  name for both TUI and app-server without introducing a TUI dependency.

The registration registry should be separate from `ThreadState`. It is
connection-oriented state keyed by `ConnectionId`, with a reverse index by
`ThreadId` for fan-out and one optional request-user-input responder per thread.

## Delivery stages

1. Add read-only registration, initial snapshots, model-response events,
   terminal turn events, session updates, queue bounds, and unregister cleanup.
2. Gate `turn/start` and `turn/steer` behind `userInput.send`.
3. Add prompt observation and close notifications without response rights.
4. Add the exclusive `requestUserInput` lease, response validation, disconnect
   fallback, and delegated UI state.
5. Update `scripts/xedoc-session` with a long-running `connect` mode as the
   reference session script.

Each stage preserves existing ordinary app-server clients. The first coherent
implementation should stop after stage 1 if the full change would exceed the
repository's review-size guidance.
