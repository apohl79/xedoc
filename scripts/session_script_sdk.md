# Session Script SDK

`session_script_sdk.py` is a dependency-free Python client for experimental
app-server session scripts. A session script is a host-managed child process
that is attached to one loaded root thread. It can observe selected activity,
read a bounded session snapshot, optionally send ordinary user input, and,
when exclusively granted that capability, answer `request_user_input` prompts.

The SDK also provides a small WebSocket JSON-RPC transport for ordinary
app-server clients. A WebSocket client cannot register as a session script:
registration requires the immutable host scope supplied to a child process.

## Contents

- [Choose an integration mode](#choose-an-integration-mode)
- [Install and configure a script](#install-and-configure-a-script)
- [Write a persistent script](#write-a-persistent-script)
- [Read state and receive events](#read-state-and-receive-events)
- [Send ordinary user input](#send-ordinary-user-input)
- [Observe and answer prompts](#observe-and-answer-prompts)
- [Build a plugin session extension](#build-a-plugin-session-extension)
- [SDK API](#sdk-api)
- [Limits and security boundaries](#limits-and-security-boundaries)

## Choose an integration mode

| Mode | How it starts | What it is for |
| --- | --- | --- |
| Configured session script | Xedoc starts one child for each loaded root thread from `config.toml`. | Automation that is trusted by the local configuration. Multiple configured scripts may attach to the same thread. |
| Plugin session extension | Xedoc discovers an extension declared in `plugin.json`, asks for approval, runs its one-shot setup, then starts its persistent child. | Installable plugin functionality with reviewed commands and requested capabilities. |
| WebSocket controller | Your process calls `SessionScriptClient.connect_websocket`. | A normal app-server JSON-RPC client or test controller. It can use the shared request primitive but cannot use `script/register`. |

All session-script API methods and notifications are experimental. Initialize
the connection with `experimentalApi: true`, which `SessionScriptClient.initialize`
does automatically.

## Install and configure a script

Place `session_script_sdk.py` next to your script, or otherwise make it
importable by the Python process that runs the script. Configure each
host-managed script in `~/.xedoc/config.toml`:

```toml
[[session_scripts]]
id = "activity-recorder"
command = ["python3", "/absolute/path/to/activity_recorder.py"]
capabilities = [
  "userInput.send",
  "prompt.requestUserInput.respond",
]
subscriptions = [
  "modelResponseDeltas",
  "modelResponseCompleted",
  "turnCompleted",
  "sessionUpdates",
  "prompts.requestUserInput",
  "prompts.extensionInteraction",
  "prompts.commandExecutionApproval",
  "prompts.fileChangeApproval",
  "prompts.permissionsApproval",
  "prompts.mcpElicitation",
]
response_timeout_ms = 10000
```

`command` is an argument vector, not a shell command. Xedoc does not evaluate
it through a shell. It starts every configured entry once for each loaded root
thread. Duplicate or blank script IDs and empty commands are ignored.

The host applies the configuration as a maximum authority:

- A script may request only configured subscriptions.
- A script receives only configured capabilities that it requests.
- `response_timeout_ms` is the maximum period for which the sole
  `request_user_input` responder may hold an open prompt. It defaults to
  10 seconds and is clamped to 100–60,000 ms.

The child receives two host-owned environment variables:

```text
XEDOC_SESSION_SCRIPT_ID=<configured script id>
XEDOC_SESSION_SCRIPT_THREAD_ID=<loaded root thread id>
```

Use them as the registration identity. They are not an authority mechanism for
an external process: the corresponding connection scope is created only by the
host when it launches the child.

## Write a persistent script

This minimal script registers for the session data and lifecycle events that it
needs, then handles notifications until Xedoc closes the child pipe.

```python
#!/usr/bin/env python3
import os
import sys

from session_script_sdk import RpcError
from session_script_sdk import SessionScriptClient


def main() -> int:
    thread_id = os.environ["XEDOC_SESSION_SCRIPT_THREAD_ID"]
    script_id = os.environ["XEDOC_SESSION_SCRIPT_ID"]
    client = SessionScriptClient.from_host_child()

    registration_id: str | None = None

    def on_notification(message: dict) -> None:
        nonlocal registration_id
        method = message.get("method")
        params = message.get("params", {})

        if method == "item/agentMessage/delta":
            print(f"model delta: {params.get('delta')}", file=sys.stderr)
        elif method == "turn/completed":
            print("turn completed", file=sys.stderr)
        elif method == "script/sessionUpdated":
            session = params["session"]
            print(
                f"session title is now {session.get('title')!r}",
                file=sys.stderr,
            )
        elif method == "script/resyncRequired" and registration_id:
            snapshot = client.read(registration_id)["snapshot"]
            print(f"resynced at revision {snapshot['revision']}", file=sys.stderr)

    client.set_notification_handler(on_notification)
    try:
        client.initialize(
            client_name="activity-recorder",
            title="Activity recorder",
            version="1.0.0",
        )
        registered = client.register(
            thread_id=thread_id,
            script_id=script_id,
            name="Activity recorder",
            version="1.0.0",
            subscriptions={
                "modelResponseDeltas": True,
                "modelResponseCompleted": False,
                "turnCompleted": True,
                "prompts": [],
                "sessionUpdates": True,
            },
            requested_capabilities=[],
        )
        registration_id = registered["registrationId"]
        snapshot = registered["snapshot"]
        print(
            f"attached to {snapshot['session']['cwd']}",
            file=sys.stderr,
        )

        while True:
            client.handle_message(client.receive_message())
    except RpcError as error:
        # Closing the thread or shutting down Xedoc closes the child pipe.
        print(f"session script disconnected: {error}", file=sys.stderr)
        return 0
    finally:
        client.close()


if __name__ == "__main__":
    raise SystemExit(main())
```

`initialize` sends `initialize` and `initialized` in the required order.
`register` returns only after the host has established the script registration
and assembled its initial snapshot.

Use `stderr` for diagnostics. The child’s `stdout` is the JSONL RPC channel,
so ordinary output on `stdout` corrupts the protocol.

## Read state and receive events

### Initial and replacement state

`register` and `read(registration_id)` return a bounded snapshot:

```json
{
  "revision": 4,
  "session": {
    "sessionId": "session_...",
    "threadId": "thread_...",
    "title": "Investigate notifications",
    "projectName": "xedoc",
    "projectRoot": "/work/xedoc",
    "cwd": "/work/xedoc"
  },
  "thread": {
    "status": "idle",
    "canAcceptDirectInput": true
  },
  "turn": null,
  "pendingPrompts": []
}
```

The `session` object contains the current title, project name, project root,
and working directory. `thread` reports the current thread state and whether
ordinary direct input is currently accepted. `turn` is the current turn
projection when a turn is active; its item list is intentionally omitted. This
keeps the script view bounded and prevents it from becoming an unbounded
history API.

With `sessionUpdates: true`, Xedoc sends `script/sessionUpdated`:

```json
{
  "method": "script/sessionUpdated",
  "params": {
    "registrationId": "registration_...",
    "revision": 5,
    "session": { "title": "New title", "cwd": "/work/xedoc" }
  }
}
```

Treat that `session` value as a replacement value for your cached session data.

If event delivery was deferred while registration was becoming ready, Xedoc
sends `script/resyncRequired`. Call `read(registration_id)` and replace all
cached snapshot state; do not try to reconstruct missed state from events.

### Subscriptions

Use these keys in `register(..., subscriptions=...)`. Every `true` value or
prompt kind must also be allowed by the host configuration or plugin grant.

| Registration field | Notification(s) delivered |
| --- | --- |
| `modelResponseDeltas: true` | `item/agentMessage/delta` while a model message streams. |
| `modelResponseCompleted: true` | `item/completed` for a completed `agentMessage` item. |
| `turnCompleted: true` | `turn/completed` when a turn reaches a terminal state. |
| `sessionUpdates: true` | `script/sessionUpdated` when the session projection changes. |
| `prompts: ["requestUserInput"]` | `script/promptOpened` and `script/promptClosed` for a `request_user_input` tool prompt. |
| `prompts: ["extensionInteraction"]` | Prompt lifecycle events for extension and model-router interaction surfaces. |
| `prompts: ["commandExecutionApproval"]` | Prompt lifecycle events for command-execution approval. |
| `prompts: ["fileChangeApproval"]` | Prompt lifecycle events for file-change approval. |
| `prompts: ["permissionsApproval"]` | Prompt lifecycle events for permissions approval. |
| `prompts: ["mcpElicitation"]` | Prompt lifecycle events for MCP elicitation. |

Every registered script also receives `turn/started` for its thread. The
special `script/*` notifications include the script’s `registrationId`; normal
model and turn notifications keep their existing app-server payload shape.

### Multiple scripts

You can configure or approve multiple scripts for the same root thread. Every
connection has a separate registration ID, snapshot revision, subscriptions,
and granted capabilities.

There is one deliberate exception: at most one script on a thread may receive
`prompt.requestUserInput.respond`. Registration fails when another script
already owns the responder capability. Other scripts may still observe that
prompt, but they receive `canRespond: false` and no lease.

## Send ordinary user input

Request `userInput.send` in both host policy and `register`. When it is
granted, the script may call only `turn/start` and `turn/steer` for its
registered thread. It cannot change the model, cwd, sandbox, permissions,
environment, provider, or other turn settings.

Start an ordinary turn only when the latest snapshot says
`thread.canAcceptDirectInput` is true:

```python
started = client.request(
    "turn/start",
    {
        "threadId": thread_id,
        "clientUserMessageId": "recorder-follow-up-001",
        "input": [{"type": "text", "text": "Summarize the current status."}],
    },
)
turn_id = started["turnId"]
```

Steer the currently active regular turn with its required precondition:

```python
client.request(
    "turn/steer",
    {
        "threadId": thread_id,
        "clientUserMessageId": "recorder-steer-001",
        "expectedTurnId": turn_id,
        "input": [{"type": "text", "text": "Also include concrete next steps."}],
    },
)
```

The script cannot read arbitrary thread history, start a different thread, or
make other app-server requests after registration. A failed input request is a
normal possibility—for example, if the thread has become unavailable or the
turn ID is no longer active—so handle `RpcError`.

## Observe and answer prompts

### Observe every selected prompt class

Scripts that subscribe to a prompt class receive a bounded projection:

```json
{
  "method": "script/promptOpened",
  "params": {
    "registrationId": "registration_...",
    "promptId": "prompt_...",
    "kind": "requestUserInput",
    "threadId": "thread_...",
    "turnId": "turn_...",
    "itemId": "item_...",
    "canRespond": true,
    "responseLease": "lease_...",
    "request": {
      "method": "item/tool/requestUserInput",
      "params": { "questions": [] }
    }
  }
}
```

The `request` field preserves the originating app-server request method and
parameters. It is observational for every prompt class except a leased
`requestUserInput` prompt. In particular, scripts cannot approve router,
extension, command, file-change, permission, or MCP-elicitation prompts.

The prompt is terminally resolved by `script/promptClosed`. Its `reason` is
one of `answered`, `cancelled`, `expired`, `turnEnded`,
`responderDisconnected`, or `superseded`. Remove the prompt from local state
when this notification arrives.

### Answer `request_user_input`

Only the script that holds `prompt.requestUserInput.respond` receives both
`canRespond: true` and a `responseLease`. Use those values exactly once:

```python
def on_notification(message: dict) -> None:
    if message.get("method") != "script/promptOpened":
        return

    prompt = message["params"]
    if (
        prompt.get("kind") != "requestUserInput"
        or not prompt.get("canRespond")
    ):
        return

    client.respond(
        registration_id=registration_id,
        prompt_id=prompt["promptId"],
        response_lease=prompt["responseLease"],
        response={
            "kind": "requestUserInput",
            "answers": {
                "confirmation": {"answers": ["Yes"]},
                "reason": {"answers": ["Proceed with the default."]},
            },
        },
    )
```

The answer map must use the question IDs and allowed answer values supplied in
the prompt request. Xedoc validates the answer before accepting it. A missing,
expired, disconnected, or incorrect lease is rejected. If the response timeout
elapses, Xedoc resumes its normal prompt handling without the script’s answer.

## Build a plugin session extension

A plugin can package a reviewable session extension in `plugin.json`:

```json
{
  "name": "Signal helper",
  "version": "1.0.0",
  "extensions": [
    {
      "id": "signal-helper",
      "entrypoint": "./signal_helper.py",
      "requestedCapabilities": [
        "userInput.send",
        "prompt.requestUserInput.respond"
      ],
      "commands": [
        {
          "name": "signal",
          "description": "Send a signal to the current session."
        }
      ]
    }
  ]
}
```

Extension IDs and command names are stable identifiers: begin with an ASCII
letter or digit and then contain only ASCII letters, digits, `-`, or `_`.
`entrypoint` is resolved relative to the plugin root and must use the plugin
manifest path form (for example, `./signal_helper.py`). Command names omit the
leading slash.

### Extension lifecycle

For each loaded root thread, Xedoc:

1. Discovers plugin extension declarations.
2. Shows the user the extension’s plugin, commands, and requested access.
3. Lets the user choose **Approve for this session**, **Approve always**, or
   **Deny**.
4. Runs the extension’s one-shot setup flow.
5. Grants the approved capabilities and starts the persistent child process.
6. Publishes the extension’s declared slash commands.

An **Approve always** grant is keyed to the plugin identity, extension ID, and
a digest of the declared entrypoint, commands, requested capabilities, and
bounded plugin-manifest evidence. Changing that declaration requires a new
review. The entrypoint must be directly executable because Xedoc launches it
without a shell. The host-scoped persistent child uses the same environment
variables and `SessionScriptClient.from_host_child` flow as a configured script.

After activation, a normal controller connection can discover and invoke
commands with the generic SDK request primitive. A registered session-script
child cannot call these extension-control methods:

```python
commands = client.request(
    "sessionExtension/list",
    {"threadId": thread_id},
)["commands"]

client.request(
    "sessionExtension/command/invoke",
    {
        "threadId": thread_id,
        "extensionId": "signal-helper",
        "command": "signal",
        "arguments": ["deploy-ready"],
    },
)
```

Controllers receive `sessionExtension/commandsUpdated` whenever the enabled
command set is replaced after activation or disablement.

To disable an enabled extension for the current thread, invoke one of its
declared commands with `--disable` as the first argument. Xedoc stops the
persistent child and publishes a replacement command set; it does not dispatch
that invocation to the extension entrypoint.

### One-shot extension protocol

Before the persistent process starts, the same `entrypoint` is invoked as a
one-shot `xedoc.script/v1` program. It receives one JSON request on `stdin`,
writes one JSON response on `stdout`, and then exits.

The one-shot methods are:

| Method | Purpose |
| --- | --- |
| `extension.setup.open` | Open and complete setup before the persistent child starts. |
| `extension.command.invoke` | Handle one approved extension slash command. |
| `interaction.respond` | Continue a form, confirmation, or other declarative interaction returned by the previous invocation. |

The request context includes `extensionId`, `pluginId`, `threadId`, and the
current session thread ID and cwd. Return a `complete` result for a finished
operation, or return an `interaction` result with a declarative surface and
continuation. Xedoc renders the surface, collects the user response, and
invokes `interaction.respond`; the extension does not implement its own
approval renderer.

This structure lets an extension use a one-shot, bounded setup or command flow
while its persistent child uses the SDK for session events and approved actions.
See `session_extension_test_entrypoint.py` for a complete dual-mode reference
implementation.

## SDK API

`SessionScriptClient` intentionally exposes both high-level session helpers
and the JSON-RPC primitive for existing app-server methods.

| API | Description |
| --- | --- |
| `SessionScriptClient.from_host_child()` | Connect through the host-provided JSONL stdin/stdout pipe. Use this in configured scripts and approved extension children. |
| `SessionScriptClient.connect_websocket(endpoint, timeout)` | Connect to a `ws://` app-server endpoint with an explicit port. This is a normal controller transport, not a way to obtain session-script authority. |
| `initialize(client_name, title, version)` | Initialize the connection with experimental API support and send `initialized`. |
| `register(thread_id, script_id, name, version, subscriptions, requested_capabilities)` | Register the host-scoped child and return `registrationId`, granted capabilities, and a snapshot. |
| `read(registration_id)` | Return a fresh bounded snapshot. |
| `respond(registration_id, prompt_id, response_lease, response)` | Answer a valid leased `request_user_input` prompt. |
| `unregister(registration_id)` | Remove this connection’s registration. Closing the connection also removes it. |
| `request(method, params)` | Send any permitted JSON-RPC request and return its object result. Raises `RpcError` for a JSON-RPC error. |
| `request_error(method, params)` | Send a request expected to fail and return its error message. Useful in tests. |
| `set_notification_handler(handler)` | Set a callback for server notifications. |
| `set_server_request_handler(handler)` | Set a callback for incoming JSON-RPC server requests. Its returned object becomes the response result. Without a handler, the SDK returns JSON-RPC `-32601`. |
| `receive_message()` / `handle_message(message)` | Receive and dispatch a message manually; useful for custom event loops. |
| `send_result(id, result)` / `send_error(id, code, message)` | Send a manual response to an incoming JSON-RPC server request. |
| `close()` | Close the WebSocket transport or release the child transport. |

`RpcError` covers invalid JSON-RPC, oversize messages, server errors returned
from `request`, and an unexpected closed transport.

## Limits and security boundaries

- Session scripts attach only to loaded root threads. They do not attach to
  subagent threads.
- A child may register only once, and only with the host-provided script ID and
  thread ID.
- Registration exposes a bounded snapshot, not arbitrary thread history.
- The response lease makes `request_user_input` answering exclusive and
  time-bounded; all other prompt projections are observe-only.
- A registered script has no general app-server authority. Apart from its
  `script/*` methods, it can use only the narrowly granted direct-input
  operations for its own thread.
- JSONL and WebSocket messages are limited to 1 MiB. The SDK also bounds
  unmatched buffered JSON-RPC responses to 64.
- The SDK WebSocket transport supports `ws://` URLs with an explicit port. It
  is deliberately dependency-free and does not implement TLS (`wss://`).

For executable end-to-end examples, see:

- `session_script_test_extension.py` — registered-script subscriptions,
  snapshots, direct input, prompt response, and resync behavior.
- `session_extension_test_entrypoint.py` — one-shot extension setup and
  command continuations plus the persistent extension child.
- `test_session_script_tmux.sh` — full lifecycle coverage through the TUI,
  app-server, plugin activation, router interaction, and MCP elicitation.
