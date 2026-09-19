#!/usr/bin/env bash

# Remote app-server acceptance harness for host-managed session scripts.
#
# The tmux server pane hosts the real app-server. It starts two configured
# child scripts over their stdin/stdout JSON-RPC pipes; a regular websocket
# client only creates and updates the root thread.

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd -- "$script_dir/.." && pwd)"
readonly repo_root
readonly binary="${XEDOC_SESSION_SCRIPT_TEST_BIN:-$repo_root/bazel-bin/xedoc-rs/cli/xedoc}"
readonly extension="$script_dir/session_script_test_extension.py"
readonly session_extension="$script_dir/session_extension_test_entrypoint.py"
readonly mcp_server="$script_dir/test_session_script_mcp_server.py"
readonly mock="$script_dir/test_session_script_responses_mock.py"
readonly source_router="$script_dir/model-router/reference-router"
readonly source_router_policy="$script_dir/model-router/reference-router.policy.json"
readonly keep_tmp_dir="${XEDOC_SESSION_SCRIPT_TEST_KEEP_DIR:-0}"
readonly install_plugin_after_thread_start="${XEDOC_SESSION_SCRIPT_TEST_PLUGIN_INSTALL_AFTER_THREAD_START:-0}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/xedoc-session-script.XXXXXX")"
readonly tmp_dir
readonly runtime_home="$tmp_dir/home"
readonly artifacts="$tmp_dir/artifacts"
readonly mock_port_file="$artifacts/mock-port"
readonly mock_requests="$artifacts/responses.jsonl"
readonly model_catalog="$artifacts/models.json"
readonly router_dir="$runtime_home/model-router"
readonly router="$router_dir/reference-router"
readonly router_policy="$router_dir/reference-router.policy.json"
readonly controller_log="$artifacts/controller.jsonl"
readonly controller_ready="$artifacts/controller-ready.json"
readonly secondary_ready="$artifacts/secondary-ready.json"
readonly responder_log="$artifacts/responder.jsonl"
readonly observer_log="$artifacts/observer.jsonl"
readonly session_extension_log="$artifacts/session-extension.jsonl"
readonly mcp_log="$artifacts/mcp.jsonl"
readonly start_file="$artifacts/start"
readonly primary_thread_file="$artifacts/primary-thread.json"
readonly plugin_root="$runtime_home/plugins/cache/local-test/session-script-e2e/local"
readonly plugin_entrypoint="$plugin_root/extensions/signal-bridge"
readonly deferred_plugin_root="$tmp_dir/deferred-plugin"
readonly extension_id="session-script-e2e:signal"
python_bin="${XEDOC_SESSION_SCRIPT_TEST_PYTHON:-python3}"
if ! command -v "$python_bin" >/dev/null 2>&1 ||
  ! "$python_bin" -c 'import sys; raise SystemExit(sys.version_info < (3, 10))'
then
  command -v uv >/dev/null 2>&1 || {
    printf 'FAIL: Python 3.10+ or uv is required\n' >&2
    exit 1
  }
  python_bin="$(uv run --frozen --project "$script_dir" python -c 'import sys; print(sys.executable)')"
fi
readonly python_bin
readonly app_server_port="$("$python_bin" - <<'PY'
import socket
with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    print(probe.getsockname()[1])
PY
)"
readonly endpoint="ws://127.0.0.1:$app_server_port"

tmux_session=""
mock_pid=""
controller_pid=""

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  if [[ -n "$tmux_session" ]]; then
    tmux list-panes -t "$tmux_session":0 -F '#{pane_id}' 2>/dev/null |
      while IFS= read -r pane; do
        tmux capture-pane -pt "$pane" -S -120 2>/dev/null || true
      done >&2
  fi
  exit 1
}

cleanup() {
  local status="$1"
  [[ -n "$tmux_session" ]] && tmux kill-session -t "$tmux_session" >/dev/null 2>&1 || true
  [[ -n "$mock_pid" ]] && kill "$mock_pid" >/dev/null 2>&1 || true
  [[ -n "$mock_pid" ]] && wait "$mock_pid" >/dev/null 2>&1 || true
  [[ -n "$controller_pid" ]] && kill "$controller_pid" >/dev/null 2>&1 || true
  [[ -n "$controller_pid" ]] && wait "$controller_pid" >/dev/null 2>&1 || true
  if [[ "$status" -eq 0 && "$keep_tmp_dir" != 1 ]]; then
    rm -rf "$tmp_dir"
  else
    printf 'Session-script artifacts retained at %s\n' "$tmp_dir" >&2
  fi
}

trap 'cleanup "$?"' EXIT INT TERM HUP

wait_for_file() {
  local path="$1"
  for _ in $(seq 1 400); do
    [[ -s "$path" ]] && return
    sleep 0.05
  done
  fail "timed out waiting for $path"
}

wait_for_app_server() {
  "$python_bin" - "$app_server_port" <<'PY'
from http.client import HTTPConnection
import sys
import time

port = int(sys.argv[1])
for _ in range(400):
    try:
        connection = HTTPConnection("127.0.0.1", port, timeout=0.2)
        connection.request("GET", "/readyz")
        response = connection.getresponse()
        response.read()
        if response.status == 200:
            raise SystemExit(0)
    except OSError:
        pass
    finally:
        try:
            connection.close()
        except NameError:
            pass
    time.sleep(0.05)
raise SystemExit("timed out waiting for app-server readiness")
PY
}

wait_for_log_event() {
  local path="$1"
  local event="$2"
  for _ in $(seq 1 400); do
    if "$python_bin" - "$path" "$event" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
event = sys.argv[2]
if path.exists():
    for line in path.read_text(encoding="utf-8").splitlines():
        if json.loads(line).get("event") == event:
            raise SystemExit(0)
raise SystemExit(1)
PY
    then
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for $event in $path"
}

wait_for_log_event_count() {
  local path="$1"
  local event="$2"
  local expected="$3"
  for _ in $(seq 1 400); do
    if "$python_bin" - "$path" "$event" "$expected" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
event = sys.argv[2]
expected = int(sys.argv[3])
count = 0
if path.exists():
    count = sum(
        json.loads(line).get("event") == event
        for line in path.read_text(encoding="utf-8").splitlines()
    )
raise SystemExit(0 if count >= expected else 1)
PY
    then
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for $expected $event events in $path"
}

wait_for_log_field() {
  local path="$1"
  local event="$2"
  local field="$3"
  local expected="$4"
  for _ in $(seq 1 400); do
    if "$python_bin" - "$path" "$event" "$field" "$expected" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
event, field, expected = sys.argv[2:5]
if path.exists():
    for line in path.read_text(encoding="utf-8").splitlines():
        value = json.loads(line)
        if value.get("event") == event and value.get(field) == expected:
            raise SystemExit(0)
raise SystemExit(1)
PY
    then
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for $event with $field=$expected in $path"
}

wait_for_process_exit() {
  local pid="$1"
  for _ in $(seq 1 400); do
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for child process $pid to exit"
}

write_config() {
  local port
  port="$(<"$mock_port_file")"
  mkdir -p "$runtime_home" "$router_dir" "$plugin_root/.xedoc-plugin" \
    "$plugin_root/extensions"
  cp "$source_router" "$router"
  cp "$source_router_policy" "$router_policy"
  chmod +x "$router"
  "$python_bin" - "$router_policy" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
policy = json.loads(path.read_text(encoding="utf-8"))
policy["mode"] = "full"
policy["approval"] = "all"
path.write_text(json.dumps(policy, separators=(",", ":")), encoding="utf-8")
PY
  "$python_bin" - "$repo_root/xedoc-rs/models-manager/models.json" "$model_catalog" <<'PY'
import json
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
destination = pathlib.Path(sys.argv[2])
catalog = json.loads(source.read_text(encoding="utf-8"))
for model in catalog["models"]:
    if model["slug"] == "gpt-5.6-luna":
        model["prefer_websockets"] = False
destination.write_text(json.dumps(catalog, separators=(",", ":")), encoding="utf-8")
PY
  cat >"$runtime_home/auth.json" <<'JSON'
{"auth_mode":"apikey","OPENAI_API_KEY":"session-script-e2e-key"}
JSON
  {
    printf '#!/usr/bin/env bash\n'
    printf 'exec %q %q --log %q\n' \
      "$python_bin" "$session_extension" "$session_extension_log"
  } >"$plugin_entrypoint"
  chmod +x "$plugin_entrypoint"
  cat >"$plugin_root/.xedoc-plugin/plugin.json" <<'JSON'
{
  "name": "session-script-e2e",
  "version": "1.0.0",
  "description": "Session extension lifecycle E2E fixture",
  "extensions": [
    {
      "id": "signal",
      "entrypoint": "./extensions/signal-bridge",
      "commands": [
        {
          "name": "signal",
          "description": "Exercise session extension lifecycle."
        }
      ],
      "requestedCapabilities": ["session.observe", "userInput.send"]
    }
  ]
}
JSON
  cat >"$runtime_home/config.toml" <<EOF
model = "gpt-5.6-luna"
model_provider = "openai"
model_catalog_json = "$model_catalog"
suppress_unstable_features_warning = true
sandbox_mode = "read-only"
approval_policy = "on-request"
auto_session_name = false
openai_base_url = "http://127.0.0.1:$port/v1"

[features]
model_router = true
request_permissions_tool = true
plugins = true

[plugins."session-script-e2e@local-test"]
enabled = true

[mcp_servers.session_script]
command = "$python_bin"
args = ["$mcp_server", "--log", "$mcp_log"]
startup_timeout_sec = 10
tool_timeout_sec = 10

[model_router]
script = ["$python_bin", "$router"]
decision_timeout_ms = 3000
interaction_timeout_ms = 10000

[[session_scripts]]
id = "session-script-responder"
command = ["$python_bin", "$extension", "--log", "$responder_log", "child", "--role", "responder", "--start-file", "$start_file", "--primary-thread-file", "$primary_thread_file"]
capabilities = ["userInput.send", "prompt.requestUserInput.respond"]
subscriptions = ["modelResponseDeltas", "modelResponseCompleted", "userMessages", "turnCompleted", "sessionUpdates", "prompts.requestUserInput", "prompts.extensionInteraction", "prompts.commandExecutionApproval", "prompts.fileChangeApproval", "prompts.permissionsApproval", "prompts.mcpElicitation"]
response_timeout_ms = 500

[[session_scripts]]
id = "session-script-observer"
command = ["$python_bin", "$extension", "--log", "$observer_log", "child", "--role", "observer", "--primary-thread-file", "$primary_thread_file"]
subscriptions = ["modelResponseDeltas", "modelResponseCompleted", "userMessages", "turnCompleted", "sessionUpdates", "prompts.requestUserInput", "prompts.extensionInteraction", "prompts.commandExecutionApproval", "prompts.fileChangeApproval", "prompts.permissionsApproval", "prompts.mcpElicitation"]
EOF
}

assert_evidence() {
  "$python_bin" - \
    "$responder_log" \
    "$observer_log" \
    "$controller_log" \
    "$mock_requests" \
    "$session_extension_log" \
    "$mcp_log" \
    "$repo_root" \
    "$runtime_home" \
    "$runtime_home/session-extension-grants.json" \
    "$extension_id" <<'PY'
import json
import pathlib
import sys

def events(path):
    lines = pathlib.Path(path).read_text(encoding="utf-8").splitlines()
    assert 2 <= len(lines) <= 256, lines
    return [json.loads(line) for line in lines]

responder, observer, controller = map(events, sys.argv[1:4])
requests = [json.loads(line) for line in pathlib.Path(sys.argv[4]).read_text().splitlines()]
session_extension = events(sys.argv[5])
mcp = events(sys.argv[6])
repo_root = pathlib.Path(sys.argv[7]).resolve()
runtime_home = pathlib.Path(sys.argv[8]).resolve()
grants = json.loads(pathlib.Path(sys.argv[9]).read_text(encoding="utf-8"))
extension_id = sys.argv[10]

assert len(requests) == 6, requests
assert requests[0]["hasPromptMarker"], requests
assert any(request["hasSteerMarker"] for request in requests), requests
expected_outputs = {
    "session-script-input-primary",
    "session-script-input-fallback",
    "session-script-permissions",
    "session-script-patch",
    "session-script-command",
}
assert set(requests[-1]["callOutputIds"]) == expected_outputs, requests

controller_names = [entry["event"] for entry in controller]
assert any(
    entry["event"] == "externalRegisterRejected" and entry["rejected"]
    for entry in controller
), controller
assert "controllerTurnCompleted" in controller_names, controller
assert controller_names.count("threadArchived") == 2, controller
primary_thread = next(
    entry["threadId"] for entry in controller if entry["event"] == "threadStarted"
)
secondary_thread = next(
    entry["threadId"] for entry in controller if entry["event"] == "idleThreadReady"
)
assert primary_thread != secondary_thread, controller
handled_requests = {
    entry["method"] for entry in controller if entry["event"] == "serverRequest"
}
assert handled_requests == {
    "item/extensionInteraction/request",
    "item/tool/requestUserInput",
    "item/permissions/requestApproval",
    "item/fileChange/requestApproval",
    "item/commandExecution/requestApproval",
    "mcpServer/elicitation/request",
}, handled_requests
assert {
    entry["method"] for entry in controller if entry["event"] == "serverResponse"
} == handled_requests, controller
interaction_responses = [
    entry for entry in controller if entry["event"] == "extensionInteractionResponse"
]
approvals = [
    entry
    for entry in interaction_responses
    if entry["extensionId"] == "session-extension-approval"
]
assert len(approvals) == 1 and approvals[0]["actionId"] == "approve-always", approvals
setup_responses = [
    entry
    for entry in interaction_responses
    if entry["extensionId"] == extension_id and entry["surfaceType"] == "form"
]
assert len(setup_responses) >= 2, setup_responses
assert all(entry["actionId"] == "save-setup" for entry in setup_responses), setup_responses
assert all(entry["valueKeys"] == ["api-token"] for entry in setup_responses), setup_responses
assert all(
    entry["sensitiveFields"] == [{"id": "api-token", "defaultEmpty": True}]
    for entry in setup_responses
), setup_responses
command_responses = [
    entry
    for entry in interaction_responses
    if entry["extensionId"] == extension_id
    and entry["surfaceType"] == "confirmation"
]
assert len(command_responses) == 1, command_responses
assert command_responses[0]["actionId"] == "continue-command", command_responses
assert "session-script-sensitive-value" not in pathlib.Path(sys.argv[3]).read_text(
    encoding="utf-8"
)
listed = [entry for entry in controller if entry["event"] == "extensionListed"]
primary_lists = [entry["commands"] for entry in listed if entry["threadId"] == primary_thread]
secondary_lists = [
    entry["commands"] for entry in listed if entry["threadId"] == secondary_thread
]
expected_command = {
    "extensionId": extension_id,
    "name": "signal",
    "description": "Exercise session extension lifecycle.",
}
assert primary_lists and all(
    commands == [expected_command] for commands in primary_lists
), primary_lists
assert secondary_lists and all(
    commands == [expected_command] for commands in secondary_lists
), secondary_lists
assert any(
    entry["event"] == "extensionInvoked"
    and entry["threadId"] == primary_thread
    and entry["arguments"] == ["alpha", "beta"]
    for entry in controller
), controller
assert any(
    entry["event"] == "extensionInvoked"
    and entry["threadId"] == primary_thread
    and entry["arguments"] == ["off"]
    for entry in controller
), controller
assert any(entry["event"] == "mcpToolCalled" for entry in controller), controller

expected_prompt_methods = {
    "requestUserInput": "item/tool/requestUserInput",
    "extensionInteraction": "item/extensionInteraction/request",
    "commandExecutionApproval": "item/commandExecution/requestApproval",
    "fileChangeApproval": "item/fileChange/requestApproval",
    "permissionsApproval": "item/permissions/requestApproval",
    "mcpElicitation": "mcpServer/elicitation/request",
}
for name, stream, responder_expected in (
    ("responder", responder, True),
    ("observer", observer, False),
):
    names = [entry["event"] for entry in stream]
    assert names.count("registered") == 1 and names.count("read") == 1, (name, names)
    snapshot = next(entry for entry in stream if entry["event"] == "registrationSnapshot")
    assert snapshot["revision"] >= 1, snapshot
    assert pathlib.Path(snapshot["cwd"]).resolve() == repo_root, snapshot
    assert pathlib.Path(snapshot["projectRoot"]).resolve() == repo_root, snapshot
    assert snapshot["projectName"] == repo_root.name, snapshot
    assert snapshot["sessionId"] and snapshot["threadId"], snapshot
    assert isinstance(snapshot["canAcceptDirectInput"], bool), snapshot
    read = next(entry for entry in stream if entry["event"] == "read")
    assert read["revision"] > snapshot["revision"], (snapshot, read)
    assert read["pendingPromptCount"] == 0, read
    assert any(entry["event"] == "restricted" and entry["rejected"] for entry in stream), stream
    assert any(
        entry["event"] == "preRegistrationRejected" and entry["rejected"]
        for entry in stream
    ), stream
    assert any(
        entry["event"] == "turnSettingsRejected" and entry["rejected"] for entry in stream
    ), stream
    registration = next(entry for entry in stream if entry["event"] == "registered")
    expected_capabilities = (
        {"userInput.send", "prompt.requestUserInput.respond"}
        if responder_expected
        else set()
    )
    assert set(registration["grantedCapabilities"]) == expected_capabilities, registration
    assert "sessionUpdated" in names, (name, names)
    assert any(
        entry["event"] == "sessionUpdated"
        and pathlib.Path(entry["cwd"]).resolve() == runtime_home
        for entry in stream
    ), stream
    assert any(
        entry["event"] == "sessionUpdated" and entry["title"] == "Session script E2E"
        for entry in stream
    ), stream
    assert sum(entry["event"] == "secondaryHostSkipped" for entry in stream) == 1, stream
    opened = [entry for entry in stream if entry["event"] == "promptOpened"]
    assert len(opened) >= 7, opened
    kinds = [entry["kind"] for entry in opened]
    assert kinds.count("requestUserInput") == 2, kinds
    assert set(kinds) == set(expected_prompt_methods), kinds
    assert kinds.count("mcpElicitation") == 1, kinds
    assert kinds.count("extensionInteraction") >= 1, kinds
    assert kinds.count("commandExecutionApproval") == 1, kinds
    assert kinds.count("fileChangeApproval") == 1, kinds
    assert kinds.count("permissionsApproval") == 1, kinds
    for entry in opened:
        assert entry["requestMethod"] == expected_prompt_methods[entry["kind"]], entry
        expected_response_rights = responder_expected and entry["kind"] == "requestUserInput"
        assert entry["canRespond"] is expected_response_rights, entry
        assert entry["hasResponseLease"] is expected_response_rights, entry
    prompt_reads = [entry for entry in stream if entry["event"] == "promptRead"]
    assert len(prompt_reads) == len(opened), prompt_reads
    assert all(entry["pendingPromptCount"] in (0, 1) for entry in prompt_reads), prompt_reads
    assert any(
        entry["kind"] == "requestUserInput"
        and entry["activeTurn"]
        and entry["pendingPromptCount"] == 1
        for entry in prompt_reads
    ), prompt_reads
    closed_reasons = [
        entry["reason"] for entry in stream if entry["event"] == "promptClosed"
    ]
    assert closed_reasons.count("answered") == len(opened) - 1, closed_reasons
    assert closed_reasons.count("expired") == 1, closed_reasons
    deltas = [entry["delta"] for entry in stream if entry["event"] == "delta"]
    assert deltas == ["session-script partial ", "answer"], deltas
    completed = [entry for entry in stream if entry["event"] == "completed"]
    assert completed == [
        {
            "event": "completed",
            "itemType": "userMessage",
            "text": None,
            "clientId": "session-script-e2e",
            "content": [{"type": "text", "text": "SESSION_SCRIPT_E2E_PROMPT", "text_elements": []}],
        },
        {
            "event": "completed",
            "itemType": "userMessage",
            "text": None,
            "clientId": "session-script-e2e-steer",
            "content": [{"type": "text", "text": "SESSION_SCRIPT_E2E_STEER", "text_elements": []}],
        },
        {
            "event": "completed",
            "itemType": "agentMessage",
            "text": "session-script answer",
            "clientId": None,
            "content": None,
        },
    ], completed
    assert any(
        entry["event"] == "turnStartedNotification"
        and entry["active"]
        and entry["status"] == "inProgress"
        for entry in stream
    ), stream
    assert names.count("turnCompleted") == 1, stream
    assert names[-1] == "unregistered", (name, names)
assert any(entry["event"] == "responded" for entry in responder), responder
assert any(entry["event"] == "responseDeferred" for entry in responder), responder
assert any(
    entry["event"] == "invalidResponseRejected" and entry["rejected"]
    for entry in responder
), responder
assert not any(entry["event"] == "responded" for entry in observer), observer
turn_started = next(entry for entry in responder if entry["event"] == "turnStarted")
turn_steered = next(entry for entry in responder if entry["event"] == "turnSteered")
assert turn_started["turnId"] == turn_steered["turnId"], (turn_started, turn_steered)
responder_scope = next(entry for entry in responder if entry["event"] == "hostScope")
observer_scope = next(entry for entry in observer if entry["event"] == "hostScope")
assert responder_scope["scriptId"] == "session-script-responder", responder_scope
assert observer_scope["scriptId"] == "session-script-observer", observer_scope
assert responder_scope["threadId"] == observer_scope["threadId"], (
    responder_scope,
    observer_scope,
)
assert responder_scope["pid"] != observer_scope["pid"], (responder_scope, observer_scope)

extension_names = [entry["event"] for entry in session_extension]
assert "extensionError" not in extension_names, session_extension
setup_requests = [
    entry
    for entry in session_extension
    if entry["event"] == "oneShotRequest"
    and entry["method"] == "extension.setup.open"
]
assert {entry["threadId"] for entry in setup_requests} == {
    primary_thread,
    secondary_thread,
}, setup_requests
setup_answers = [
    entry for entry in session_extension if entry["event"] == "setupResponded"
]
assert len(setup_answers) == 2, setup_answers
assert all(entry["actionId"] == "save-setup" for entry in setup_answers), setup_answers
assert all(entry["sensitiveValueBytes"] > 0 for entry in setup_answers), setup_answers
assert any(
    entry["event"] == "commandInvoked"
    and entry["threadId"] == primary_thread
    and entry["command"] == "signal"
    and entry["arguments"] == ["alpha", "beta"]
    for entry in session_extension
), session_extension
assert any(
    entry["event"] == "commandResponded"
    and entry["threadId"] == primary_thread
    and entry["actionId"] == "continue-command"
    for entry in session_extension
), session_extension
persistent = [
    entry for entry in session_extension if entry["event"] == "persistentRegistered"
]
assert {entry["threadId"] for entry in persistent} == {
    primary_thread,
    secondary_thread,
}, persistent
assert len({entry["pid"] for entry in persistent}) == 2, persistent
assert all(entry["scriptId"] == extension_id for entry in persistent), persistent
assert all(entry["grantedCapabilities"] == ["userInput.send"] for entry in persistent), persistent
assert all(entry["hasSnapshot"] for entry in persistent), persistent
assert any(
    entry["event"] == "persistentDelta"
    and entry["threadId"] == primary_thread
    and entry["delta"] == "session-script partial "
    for entry in session_extension
), session_extension
assert any(
    entry["event"] == "persistentCompleted"
    and entry["threadId"] == primary_thread
    and entry["text"] == "session-script answer"
    for entry in session_extension
), session_extension
assert any(
    entry["event"] == "persistentUserMessage"
    and entry["threadId"] == primary_thread
    and entry["clientId"] == "session-script-e2e"
    and entry["content"]
    == [{"type": "text", "text": "SESSION_SCRIPT_E2E_PROMPT", "text_elements": []}]
    for entry in session_extension
), session_extension
assert any(
    entry["event"] == "persistentUserMessage"
    and entry["threadId"] == primary_thread
    and entry["clientId"] == "session-script-e2e-steer"
    and entry["content"]
    == [{"type": "text", "text": "SESSION_SCRIPT_E2E_STEER", "text_elements": []}]
    for entry in session_extension
), session_extension
assert any(
    entry["event"] == "persistentTurnCompleted"
    and entry["threadId"] == primary_thread
    for entry in session_extension
), session_extension
assert any(
    entry["event"] == "persistentSessionUpdated"
    and entry["threadId"] == secondary_thread
    and entry["title"] == "Secondary remains live"
    for entry in session_extension
), session_extension
assert {
    (thread_id, level, f"persistent {level} message")
    for thread_id in (primary_thread, secondary_thread)
    for level in ("info", "warning", "error")
}.issubset(
    {
        (
            entry["threadId"],
            entry["level"],
            entry["message"],
        )
        for entry in controller
        if entry["event"] == "sessionExtensionMessage"
    }
), controller
assert all(
    entry["extensionName"] == "signal"
    for entry in controller
    if entry["event"] == "sessionExtensionMessage"
), controller
assert {
    "signal extension configured",
    "signal extension on",
    "signal command completed",
    "signal extension off",
}.issubset(
    {
        entry["message"]
        for entry in controller
        if entry["event"] == "sessionExtensionMessage"
        and entry["extensionName"] == "signal"
        and entry["level"] == "info"
    }
), controller

assert any(entry["event"] == "toolCalled" for entry in mcp), mcp
assert any(
    entry["event"] == "elicitationAnswered" and entry["action"] == "decline"
    for entry in mcp
), mcp
assert len(grants) == 1, grants
assert grants == [
    {
        "plugin_id": "session-script-e2e",
        "extension_id": extension_id,
        "declaration_digest": grants[0]["declaration_digest"],
    }
], grants
PY
}

main() {
  command -v tmux >/dev/null || fail "tmux is required"
  [[ -x "$python_bin" ]] || fail "Python 3.10+ is required"
  [[ -x "$binary" ]] || fail "XEDOC_SESSION_SCRIPT_TEST_BIN is not executable: $binary"
  mkdir -p "$artifacts"
  "$python_bin" "$mock" --port-file "$mock_port_file" --request-log "$mock_requests" \
    >"$artifacts/mock.stdout.log" 2>"$artifacts/mock.stderr.log" &
  mock_pid="$!"
  wait_for_file "$mock_port_file"
  write_config
  if [[ "$install_plugin_after_thread_start" == 1 ]]; then
    mv "$plugin_root" "$deferred_plugin_root"
  fi

  tmux_session="xedoc-session-script-$RANDOM-$$"
  tmux new-session -d -x 220 -y 50 -s "$tmux_session" \
    "exec env RUST_LOG=xedoc_app_server=info XEDOC_HOME=$(printf %q "$runtime_home") OPENAI_API_KEY=session-script-e2e-key $(printf %q "$binary") app-server --listen $(printf %q "$endpoint")"
  tmux set-option -t "$tmux_session":0 remain-on-exit on
  wait_for_app_server
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" start-thread \
    --session-cwd "$repo_root" --ready-file "$controller_ready" \
    --primary-thread-file "$primary_thread_file" \
    >"$artifacts/controller.stdout.log" 2>"$artifacts/controller.stderr.log" &
  controller_pid="$!"
  wait_for_file "$controller_ready"
  if [[ "$install_plugin_after_thread_start" == 1 ]]; then
    sleep 1
    mv "$deferred_plugin_root" "$plugin_root"
    wait_for_log_event_count "$controller_log" extensionInteractionResponse 1
  fi
  local thread_id
  thread_id="$("$python_bin" - "$controller_ready" <<'PY'
import json
import pathlib
import sys
print(json.loads(pathlib.Path(sys.argv[1]).read_text())["threadId"])
PY
  )"
  wait_for_log_event "$responder_log" registered
  wait_for_log_event "$observer_log" registered
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-invoke \
    --thread-id "$thread_id" --extension-id "$extension_id" \
    --extension-command signal on
  wait_for_log_event_count "$session_extension_log" persistentRegistered 1
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-list \
    --thread-id "$thread_id"

  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" \
    start-idle-thread --session-cwd "$repo_root" --ready-file "$secondary_ready"
  local secondary_thread_id
  secondary_thread_id="$("$python_bin" - "$secondary_ready" <<'PY'
import json
import pathlib
import sys
print(json.loads(pathlib.Path(sys.argv[1]).read_text())["threadId"])
PY
)"
  wait_for_log_event_count "$responder_log" secondaryHostSkipped 1
  wait_for_log_event_count "$observer_log" secondaryHostSkipped 1
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-invoke \
    --thread-id "$secondary_thread_id" --extension-id "$extension_id" \
    --extension-command signal on
  wait_for_log_event_count "$session_extension_log" persistentRegistered 2
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-list \
    --thread-id "$secondary_thread_id"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-list \
    --thread-id "$thread_id"

  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-invoke \
    --thread-id "$thread_id" --extension-id "$extension_id" \
    --extension-command signal alpha beta
  wait_for_log_event "$session_extension_log" commandResponded
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" mcp-tool-call \
    --thread-id "$thread_id" --server session_script --tool elicit

  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" update-cwd \
    --thread-id "$thread_id" --session-cwd "$runtime_home"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" update-title \
    --thread-id "$thread_id" --name "Session script E2E"
  : >"$start_file"
  wait_for_log_event "$responder_log" unregistered
  wait_for_log_event "$observer_log" unregistered
  wait_for_log_event "$controller_log" controllerTurnCompleted
  if ! wait "$controller_pid"; then
    fail "controller exited unsuccessfully"
  fi
  controller_pid=""
  local responder_pid observer_pid primary_extension_pid secondary_extension_pid
  responder_pid="$("$python_bin" - "$responder_log" <<'PY'
import json
import pathlib
import sys
for line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    event = json.loads(line)
    if event.get("event") == "hostScope":
        print(event["pid"])
        break
PY
)"
  observer_pid="$("$python_bin" - "$observer_log" <<'PY'
import json
import pathlib
import sys
for line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    event = json.loads(line)
    if event.get("event") == "hostScope":
        print(event["pid"])
        break
PY
)"
  primary_extension_pid="$("$python_bin" - "$session_extension_log" "$thread_id" <<'PY'
import json
import pathlib
import sys
for line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    event = json.loads(line)
    if event.get("event") == "persistentRegistered" and event.get("threadId") == sys.argv[2]:
        print(event["pid"])
        break
PY
)"
  secondary_extension_pid="$("$python_bin" - "$session_extension_log" "$secondary_thread_id" <<'PY'
import json
import pathlib
import sys
for line in pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").splitlines():
    event = json.loads(line)
    if event.get("event") == "persistentRegistered" and event.get("threadId") == sys.argv[2]:
        print(event["pid"])
        break
PY
)"
  wait_for_process_exit "$responder_pid"
  wait_for_process_exit "$observer_pid"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-invoke \
    --thread-id "$thread_id" --extension-id "$extension_id" \
    --extension-command signal off
  wait_for_process_exit "$primary_extension_pid"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-list \
    --thread-id "$thread_id"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" archive-thread \
    --thread-id "$thread_id"
  kill -0 "$secondary_extension_pid" >/dev/null 2>&1 ||
    fail "secondary extension child exited when the primary was disabled"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" update-title \
    --thread-id "$secondary_thread_id" --name "Secondary remains live"
  wait_for_log_field "$session_extension_log" persistentSessionUpdated title \
    "Secondary remains live"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" extension-list \
    --thread-id "$secondary_thread_id"
  "$python_bin" "$extension" --endpoint "$endpoint" --log "$controller_log" archive-thread \
    --thread-id "$secondary_thread_id"
  wait_for_process_exit "$secondary_extension_pid"
  assert_evidence
  printf 'PASS: host-managed session-script and extension tmux integration matrix\n'
}

main "$@"
