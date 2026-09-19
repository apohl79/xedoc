#!/usr/bin/env bash

# Standalone acceptance matrix for the packaged scripted model router.
#
# This intentionally drives the real TUI through tmux.  It never invokes the
# router script directly, never starts `xedoc app-server`, and never uses
# `--remote`.  Each model assertion comes from the request received by the
# local Responses mock, after Xedoc has rendered and handled the interaction.

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
readonly script_dir
repo_root="$(cd -- "$script_dir/.." && pwd)"
readonly repo_root
readonly source_router="$repo_root/scripts/model-router/reference-router"
readonly source_policy="$repo_root/scripts/model-router/reference-router.policy.json"
readonly source_embedder="$repo_root/scripts/model-router/reference-router-embedder.py"
readonly source_semantic_policy="$repo_root/scripts/model-router/reference-router.semantic-policy.json"
readonly source_catalog="$repo_root/xedoc-rs/models-manager/models.json"
readonly mock_server="$script_dir/model_router_tmux_responses_mock.py"
readonly initial_model="gpt-5.6-luna"
readonly initial_effort="low"
readonly keep_tmp_dir="${XEDOC_TMUX_TEST_KEEP_DIR:-0}"
readonly binary="${XEDOC_TMUX_TEST_BIN:-$repo_root/bazel-bin/xedoc-rs/cli/xedoc}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/xedoc-model-router-tmux.XXXXXX")"
readonly tmp_dir
readonly package_dir="$tmp_dir/package"
readonly runtime_home="$tmp_dir/runtime-home"
readonly artifact_dir="$tmp_dir/artifacts"
readonly policy_path="$runtime_home/model-router/reference-router.policy.json"
readonly router_diagnostics="$runtime_home/model-router/reference-router.diagnostics.jsonl"
readonly request_log="$artifact_dir/responses.jsonl"
readonly scenario_log="$artifact_dir/scenarios.tsv"
readonly mock_port_file="$artifact_dir/mock-port"
readonly hold_response_file="$artifact_dir/release-held-root"
readonly permission_hook_log="$artifact_dir/model-router-permission-hook.jsonl"
readonly state_db="$runtime_home/state_5.sqlite"
readonly router_runtime="${XEDOC_TMUX_ROUTER_RUNTIME:-}"

tmux_session=""
mock_pid=""
fail() {
  local message="$1"
  capture_pane >"$artifact_dir/failure-pane.log" 2>&1 || true
  {
    printf 'FAIL: %s\n' "$message"
    printf '\n=== tmux pane ===\n'
    capture_pane || true
    printf '\n=== pane process ===\n'
    if [[ -n "$tmux_session" ]]; then
      tmux list-panes -t "$tmux_session":0 -F \
        'pid=#{pane_pid} command=#{pane_current_command} start=#{pane_start_command}' || true
    fi
    printf '\n=== Responses request count ===\n'
    wc -l <"$request_log" 2>/dev/null || true
  } >&2
  exit 1
}

cleanup() {
  local status="$1"
  if [[ -n "$tmux_session" ]]; then
    capture_pane >"$artifact_dir/final-pane.log" 2>&1 || true
    tmux kill-session -t "$tmux_session" >/dev/null 2>&1 || true
  fi
  if [[ -n "$mock_pid" ]]; then
    kill "$mock_pid" >/dev/null 2>&1 || true
    wait "$mock_pid" >/dev/null 2>&1 || true
  fi
  if [[ "$status" -eq 0 && "$keep_tmp_dir" != "1" ]]; then
    rm -rf "$tmp_dir"
  else
    printf 'Standalone router tmux artifacts retained at %s\n' "$tmp_dir" >&2
  fi
}

trap 'cleanup "$?"' EXIT INT TERM HUP

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

runtime_python() {
  if [[ -x "$router_runtime/python/python.exe" ]]; then
    printf '%s\n' "$router_runtime/python/python.exe"
  else
    printf '%s\n' "$router_runtime/python/bin/python3"
  fi
}

record_scenario() {
  printf '%s\t%s\n' "$1" "$2" >>"$scenario_log"
}

capture_pane() {
  if [[ -z "$tmux_session" ]]; then
    return
  fi
  tmux capture-pane -pt "$tmux_session":0.0 -S -240
}

capture_viewport() {
  tmux capture-pane -pt "$tmux_session":0.0
}

wait_for_pane() {
  local expected="$1"
  local attempts="${2:-300}"
  local pane=""
  for _ in $(seq 1 "$attempts"); do
    pane="$(capture_viewport)"
    if [[ "$pane" == *"$expected"* ]]; then
      return
    fi
    sleep 0.1
  done
  printf '%s\n' "$pane" >&2
  fail "timed out waiting for pane text: $expected"
}

wait_for_pane_absent() {
  local unexpected="$1"
  local attempts="${2:-30}"
  local pane=""
  for _ in $(seq 1 "$attempts"); do
    pane="$(capture_viewport)"
    if [[ "$pane" != *"$unexpected"* ]]; then
      return
    fi
    sleep 0.1
  done
  printf '%s\n' "$pane" >&2
  fail "pane unexpectedly retained text: $unexpected"
}

send_key() {
  local key="$1"
  if [[ "$key" == "Enter" ]]; then
    key="C-m"
  fi
  sleep 0.2
  tmux send-keys -t "$tmux_session":0.0 "$key"
  sleep 0.12
}

send_keys() {
  local key
  for key in "$@"; do
    send_key "$key"
  done
}

send_prompt() {
  local prompt="$1"
  tmux send-keys -t "$tmux_session":0.0 -l -- "$prompt"
  sleep 0.3
  send_key Enter
}

wait_for_tui_ready() {
  local trust_confirmed=0
  local hooks_trusted=0
  local pane=""
  for _ in $(seq 1 400); do
    pane="$(capture_viewport)"
    if [[ "$pane" == *"❯"* && "$pane" == *"$initial_model · $initial_effort"* ]]; then
      return
    fi
    if [[ "$trust_confirmed" -eq 0 && "$pane" == *"Do you trust the contents of this directory?"* ]]; then
      send_key Enter
      trust_confirmed=1
    fi
    if [[ "$hooks_trusted" -eq 0 && "$pane" == *"Hooks need review"* ]]; then
      send_key Down
      send_key Enter
      hooks_trusted=1
    fi
    sleep 0.1
  done
  printf '%s\n' "$pane" >&2
  fail "timed out waiting for standalone TUI readiness"
}

assert_policy() {
  local expression="$1"
  python3 - "$policy_path" "$expression" <<'PY'
import json
import sys

policy = json.load(open(sys.argv[1], encoding="utf-8"))
expression = sys.argv[2]
if expression == "ladder":
    assert policy["ranking"]["ladder"], policy
elif expression == "baseline":
    assert policy["reportingBaseline"] is not None, policy
else:
    path, expected = expression.split("=", 1)
    actual = policy
    for part in path.split("."):
        actual = actual[int(part)] if part.isdigit() else actual[part]
    if expected == "true":
        expected_value = True
    elif expected == "false":
        expected_value = False
    elif expected == "null":
        expected_value = None
    else:
        expected_value = expected
    assert actual == expected_value, (path, actual, expected_value, policy)
PY
}

reset_policy() {
  mkdir -p "$(dirname "$policy_path")"
  cp "$source_policy" "$policy_path"
}

assert_reference_policy_contract() {
  python3 - "$source_policy" <<'PY'
import json
import sys

policy = json.load(open(sys.argv[1], encoding="utf-8"))
expected_axes = {
    "work_type": [
        (
            "group1: question, docs_analysis, packaging, operational, testing",
            1,
            "simple",
            "smart",
        ),
        (
            "group2: implementation, bug_fix, refactor, docs_authoring, orchestration, calibration",
            3,
            "simple",
            "intelligent",
        ),
        (
            "group3: research, review, diagnosis, design",
            6,
            "smart",
            "intelligent",
        ),
    ],
    "complexity": [
        ("low", 1, "simple", "smart"),
        ("medium", 3, "simple", "intelligent"),
        ("high", 6, "simple", "intelligent"),
        ("very_high", 9, "smart", "intelligent"),
    ],
    "orchestration": [
        ("none", 0, "simple", "smart"),
        ("delegate", 1, "simple", "smart"),
        ("coordination", 1, "smart", "smart"),
        ("workflow", 3, "smart", "smart"),
    ],
    "risk": [
        ("low", 1, "simple", "smart"),
        ("medium", 3, "simple", "intelligent"),
        ("high", 10, "smart", "intelligent"),
        ("very_high", 14, "intelligent", "intelligent"),
    ],
}
actual_axes = {
    axis["id"]: [
        (
            item["id"],
            item["points"],
            item["minimumModelClass"],
            item["maximumModelClass"],
        )
        for item in axis["classes"]
    ]
    for axis in policy["axes"]
}
assert actual_axes == expected_axes, actual_axes
assert policy["ranking"]["minimumScore"] == 3
assert policy["ranking"]["maximumScore"] == 35
expected_classes = ["simple"] * 5 + ["smart"] * 5 + ["intelligent"] * 5
expected_models = (
    ["gpt-5.6-luna"] * 5
    + ["gpt-5.6-terra"] * 5
    + ["gpt-5.6-sol"] * 5
)
expected_efforts = ["low", "medium", "high", "xhigh", "max"] * 3
ladder = policy["ranking"]["ladder"]
assert [entry["rank"] for entry in ladder] == list(range(1, 16))
assert [entry["class"] for entry in ladder] == expected_classes
assert [entry["model"] for entry in ladder] == expected_models
assert [entry["reasoningEffort"] for entry in ladder] == expected_efforts
assert all(entry["providerId"] == "openai" for entry in ladder)
assert policy["confidencePresets"] == {
    "strict": {
        "minimumConfidence": 0.90,
        "minimumScore": 0.50,
        "minimumMargin": 0.15,
    },
    "balanced": {
        "minimumConfidence": 0.75,
        "minimumScore": 0.35,
        "minimumMargin": 0.08,
    },
    "permissive": {
        "minimumConfidence": 0.50,
        "minimumScore": 0.20,
        "minimumMargin": 0.04,
    },
}
assert policy["confidence"] == "balanced"
assert "notConfidentPolicy" not in policy
PY
  record_scenario reference-policy-contract \
    "all axis mappings, 15 ranked slots, score domain, and confidence thresholds exact"
}

assert_script_conflict_protocol() {
  python3 - "$package_dir/xedoc-resources/model-router/reference-router" \
    "$policy_path" "$router_runtime" <<'PY'
import json
import os
import pathlib
import subprocess
import sys

router, policy_path, runtime = sys.argv[1:]
request_number = 0


def call(method, params=None, context=None):
    global request_number
    request_number += 1
    request = {
        "protocol": "xedoc.script/v1",
        "requestId": f"conflict-probe-{request_number}",
        "extension": "model-router",
        "method": method,
        "params": params or {},
        "context": context or {},
    }
    completed = subprocess.run(
        [router],
        input=json.dumps(request),
        text=True,
        capture_output=True,
        check=True,
        env={
            **os.environ,
            "XEDOC_HOME": str(pathlib.Path(policy_path).parent.parent),
            "XEDOC_ROUTER_RUNTIME": runtime,
        },
    )
    response = json.loads(completed.stdout)
    assert "error" not in response, response
    return response["result"]


def interaction(result):
    value = result["interaction"]
    return value, value["surface"]


def respond(rendered, action_id, values=None, **overrides):
    params = {
        "continuation": rendered["continuation"],
        "interactionId": rendered["id"],
        "stateRevision": rendered["stateRevision"],
        "outcome": "accepted",
        "action": {"id": action_id},
        "values": values or {},
    }
    params.update(overrides)
    return call("interaction.respond", params)


def router_context():
    policy = json.load(open(policy_path, encoding="utf-8"))
    grouped = {}
    for entry in policy["ranking"]["ladder"]:
        key = (entry["providerId"], entry["model"])
        grouped.setdefault(key, []).append(entry["reasoningEffort"])
    return {
        "currentRoute": {
            "providerId": "openai",
            "model": "gpt-5.6-luna",
            "reasoningEffort": "low",
        },
        "turn": {"scope": "root", "active": False},
        "eligibleRoutes": [
            {
                "providerId": provider_id,
                "model": model,
                "reasoningEfforts": efforts,
            }
            for (provider_id, model), efforts in grouped.items()
        ],
    }


call("settings.open")
root, _ = interaction(call("settings.open", context=router_context()))
result = respond(root, "open-mode", interactionId="wrong-settings-id")
conflict, surface = interaction(result)
assert conflict["continuation"] == "settings:root", conflict
assert surface["type"] == "menu" and "Settings conflict:" in surface["subtitle"], surface

result = respond(root, "set-mode", {"mode": "full"})
conflict, surface = interaction(result)
assert surface["type"] == "menu" and "not enabled" in surface["subtitle"], surface

mode, _ = interaction(respond(root, "open-mode"))
result = respond(mode, "set-mode", {"mode": "full"}, continuation="settings:root")
conflict, surface = interaction(result)
assert surface["type"] == "menu" and "Settings conflict:" in surface["subtitle"], surface

root, _ = interaction(call("settings.open", context=router_context()))
mode, _ = interaction(respond(root, "open-mode"))
root_after_update, _ = interaction(respond(root, "toggle-feedback"))
assert root_after_update["stateRevision"] != mode["stateRevision"], root_after_update
result = respond(mode, "set-mode", {"mode": "full"})
conflict, surface = interaction(result)
assert surface["type"] == "menu" and "Settings changed" in surface["subtitle"], surface
assert json.load(open(policy_path, encoding="utf-8"))["mode"] != "full"

root, _ = interaction(call("settings.open", context=router_context()))
root, _ = interaction(respond(root, "open-mode"))
root, _ = interaction(respond(root, "set-mode", {"mode": "full"}))
approval_form, _ = interaction(respond(root, "open-approval"))
assert [
    field["id"] for field in approval_form["surface"]["fields"]
] == ["approval"], approval_form
root, _ = interaction(
    respond(
        approval_form,
        "set-approval",
        {"approval": "all"},
    )
)
policy = json.load(open(policy_path, encoding="utf-8"))
assert policy["approval"] == "all", policy
assert policy["confidence"] == "balanced", policy
policy_form, _ = interaction(respond(root, "open-policy"))
root, _ = interaction(
    respond(policy_form, "save-policy", {"confidence": "strict"})
)
policy = json.load(open(policy_path, encoding="utf-8"))
assert policy["confidence"] == "strict", policy
settings_root, _ = interaction(respond(root, "back"))
approval, surface = interaction(
    call(
        "routing.decide",
        {"prompt": "review workflow security"},
        router_context(),
    )
)
assert approval["id"] == "route-approval", surface
settings_root, _ = interaction(respond(settings_root, "toggle-feedback"))
result = respond(approval, "approve")
fresh, surface = interaction(result)
assert fresh["id"] == "route-approval", fresh
assert fresh["stateRevision"] != approval["stateRevision"], fresh
assert any(
    "Notice:" in row["text"] and "stale" in row["text"]
    for section in surface["sections"]
    for row in section["rows"]
), surface

subagent_context = {
    **router_context(),
    "turn": {"scope": "subagent", "active": False, "routeMutable": True},
}
subagent_approval, subagent_surface = interaction(
    call(
        "routing.decide",
        {"prompt": "review workflow security"},
        subagent_context,
    )
)
assert subagent_surface["sections"][0]["rows"] == [{"text": "└ Scope: subagent"}]
subagent_refreshed, subagent_surface = interaction(
    respond(subagent_approval, "submit-override", {"work_type": "group2"})
)
assert subagent_refreshed["id"] == "route-approval", subagent_refreshed
assert subagent_surface["sections"][0]["rows"] == [{"text": "└ Scope: subagent"}]

classifier_route = {
    "providerId": "openai",
    "model": "gpt-5.6-luna",
    "reasoningEffort": "low",
}
policy = json.load(open(policy_path, encoding="utf-8"))
policy["classifierRoute"] = classifier_route
with open(policy_path, "w", encoding="utf-8") as output:
    json.dump(policy, output)
classifier_context = {
    **router_context(),
    "eligibleClassifierRoutes": [
        {
            "providerId": "openai",
            "model": "gpt-5.6-luna",
            "reasoningEfforts": ["low"],
        }
    ],
}
classifier_request = call(
    "routing.decide",
    {"prompt": "review workflow security"},
    classifier_context,
)
assert classifier_request["kind"] == "classifierRequest", classifier_request
classifier_fallback, fallback_surface = interaction(
    call(
        "routing.classifier.respond",
        {
            "continuation": classifier_request["classifier"]["continuation"],
            "error": "router classifier invocation failed: unsupported model",
            "elapsedMs": 42,
        },
        classifier_context,
    )
)
assert classifier_fallback["id"] == "route-approval", fallback_surface
assert any(
    "Notice: LLM classifier unavailable" in row["text"]
    for section in fallback_surface["sections"]
    for row in section["rows"]
), fallback_surface
accepted = respond(classifier_fallback, "approve")
assert accepted["kind"] == "route", accepted
assert "Error: LLM classifier unavailable" in accepted["decision"]["summary"], accepted

policy = json.load(open(policy_path, encoding="utf-8"))
policy["approval"] = "off"
policy["feedback"] = True
policy["mode"] = "shadow-full"
with open(policy_path, "w", encoding="utf-8") as output:
    json.dump(policy, output)
classifier_request = call(
    "routing.decide",
    {"prompt": "review workflow security"},
    classifier_context,
)
fallback = call(
    "routing.classifier.respond",
    {
        "continuation": classifier_request["classifier"]["continuation"],
        "error": "router classifier invocation failed: unsupported model",
        "elapsedMs": 42,
    },
    classifier_context,
)
assert fallback["kind"] == "route", fallback
assert fallback["decision"]["disposition"] == "shadow", fallback
assert "Error: LLM classifier unavailable" in fallback["decision"]["summary"], fallback
PY
  reset_policy
  record_scenario interaction-conflicts \
    "script rejects mismatched settings and stale settings/approval responses with fresh script surfaces"
}

assert_config_unchanged() {
  local now
  now="$(shasum -a 256 "$runtime_home/config.toml" | awk '{print $1}')"
  [[ "$now" == "$config_digest" ]] || fail "script-owned settings unexpectedly changed XEDOC_HOME/config.toml"
}

record_config_digest() {
  config_digest="$(shasum -a 256 "$runtime_home/config.toml" | awk '{print $1}')"
}

assert_request_route() {
  local marker="$1"
  local expected_model="$2"
  local expected_effort="$3"
  local excluded_thread_id="${4:-}"
  local minimum_sequence="${5:-0}"
  python3 - "$request_log" "$marker" "$expected_model" "$expected_effort" "$excluded_thread_id" "$minimum_sequence" <<'PY'
import json
import sys

path, marker, model, effort, excluded_thread_id, minimum_sequence = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
matching = [
    request
    for request in requests
    if int(request.get("sequence", 0)) > int(minimum_sequence)
    and marker in request.get("markers", [])
    and request.get("request_kind") != "model_router_classifier"
    and request.get("client_metadata", {}).get("thread_id") != excluded_thread_id
    and request.get("client_metadata", {}).get("turn_id") != "session-name"
]
assert matching, f"no Responses request contains {marker!r}"
request = matching[0]
reasoning = request.get("reasoning") or {}
actual = (request["model"], reasoning.get("effort"))
expected = (model, effort)
assert actual == expected, f"Responses request route for {marker!r}: expected={expected}, actual={actual}"
PY
}

assert_classifier_requests() {
  local marker="$1"
  local expected_count="$2"
  python3 - "$request_log" "$marker" "$expected_count" <<'PY'
import json
import sys

path, marker, expected_count = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
matching = [
    request
    for request in requests
    if marker in request.get("markers", [])
    and request.get("request_kind") == "model_router_classifier"
]
assert len(matching) == int(expected_count), (marker, len(matching), expected_count)
for request in matching:
    route = (request.get("model"), (request.get("reasoning") or {}).get("effort"))
    assert route == ("gpt-5.6-luna", "low"), route
PY
}

assert_latest_hybrid_decision() {
  local scope="$1"
  local mode="$2"
  local work_type="$3"
  local complexity="$4"
  local risk="$5"
  local orchestration="$6"
  local disposition="$7"
  python3 - "$router_diagnostics" "$scope" "$mode" "$work_type" "$complexity" "$risk" \
    "$orchestration" "$disposition" <<'PY'
import json
import sys

(path, scope, mode, work_type, complexity, risk, orchestration, disposition) = sys.argv[1:]
records = [
    json.loads(line)
    for line in open(path, encoding="utf-8")
    if line.strip()
]
decisions = [
    record
    for record in records
    if record.get("event") == "routing_decision"
    and record.get("turn", {}).get("scope") == scope
    and record.get("mode", {}).get("effective") == mode
]
assert decisions, (scope, mode)
decision = decisions[-1]
assert decision["classification"] == {
    "work_type": work_type,
    "complexity": complexity,
    "risk": risk,
    "orchestration": orchestration,
}, decision
assert decision.get("classifier") == "arctic-embed-xs", decision
assert decision.get("disposition") == disposition, decision
PY
}

set_classifier_route() {
  local mode="$1"
  python3 - "$policy_path" "$mode" <<'PY'
import json
import sys

path = sys.argv[1]
mode = sys.argv[2]
policy = json.load(open(path, encoding="utf-8"))
policy["mode"] = mode
policy["approval"] = "off"
policy["confidence"] = "permissive"
policy["classifierRoute"] = {
    "providerId": "openai",
    "model": "gpt-5.6-luna",
    "reasoningEffort": "low",
}
with open(path, "w", encoding="utf-8") as destination:
    json.dump(policy, destination, indent=2)
    destination.write("\n")
PY
}

assert_latest_router_decision() {
  local expected_steering="$1"
  local expected_prior_messages="$2"
  python3 - "$router_diagnostics" "$expected_steering" "$expected_prior_messages" <<'PY'
import json
import sys

path, expected_steering, expected_prior_messages = sys.argv[1:]
records = [
    json.loads(line)
    for line in open(path, encoding="utf-8")
    if line.strip()
]
decisions = [
    record
    for record in records
    if record.get("event") == "routing_decision"
]
assert decisions, "no routing decision diagnostic"
decision = decisions[-1]
assert decision.get("turn", {}).get("active") is False, decision
assert decision.get("steering") is (expected_steering == "true"), decision
recent_message_count = decision.get("conversation", {}).get("recentMessageCount", 0)
if expected_prior_messages == "yes":
    assert recent_message_count > 0, decision
else:
    assert recent_message_count == 0, decision
PY
}

assert_request_not_route() {
  local marker="$1"
  local forbidden_model="$2"
  local forbidden_effort="$3"
  local excluded_thread_id="${4:-}"
  local minimum_sequence="${5:-0}"
  python3 - "$request_log" "$marker" "$forbidden_model" "$forbidden_effort" "$excluded_thread_id" "$minimum_sequence" <<'PY'
import json
import sys

path, marker, model, effort, excluded_thread_id, minimum_sequence = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
matching = [
    request
    for request in requests
    if int(request.get("sequence", 0)) > int(minimum_sequence)
    and marker in request.get("markers", [])
    and request.get("client_metadata", {}).get("thread_id") != excluded_thread_id
    and request.get("client_metadata", {}).get("turn_id") != "session-name"
]
assert matching, f"no Responses request contains {marker!r}"
request = matching[0]
reasoning = request.get("reasoning") or {}
actual = (request["model"], reasoning.get("effort"))
forbidden = (model, effort)
assert actual != forbidden, (
    f"Responses request route for {marker!r}: expected anything but {forbidden}, actual={actual}"
)
PY
}

assert_child_request_count() {
  local marker="$1"
  local root_thread_id="$2"
  local root_sequence="$3"
  local expected="$4"
  python3 - "$request_log" "$marker" "$root_thread_id" "$root_sequence" "$expected" <<'PY'
import json
import sys

path, marker, root_thread_id, root_sequence, expected = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
count = sum(
    int(request.get("sequence", 0)) > int(root_sequence)
    and marker in request.get("markers", [])
    and request.get("client_metadata", {}).get("thread_id") != root_thread_id
    and request.get("client_metadata", {}).get("turn_id") != "session-name"
    for request in requests
)
assert count >= int(expected), (marker, count, expected)
PY
}

assert_exact_child_routes() {
  local marker="$1"
  local root_thread_id="$2"
  local root_sequence="$3"
  local expected_json="$4"
  python3 - "$request_log" "$marker" "$root_thread_id" "$root_sequence" "$expected_json" <<'PY'
import json
import sys

path, marker, root_thread_id, root_sequence, expected_json = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
matching = [
    request
    for request in requests
    if int(request.get("sequence", 0)) > int(root_sequence)
    and marker in request.get("markers", [])
    and request.get("client_metadata", {}).get("thread_id") != root_thread_id
    and request.get("client_metadata", {}).get("turn_id") != "session-name"
]
actual = sorted(
    (
        request["model"],
        (request.get("reasoning") or {}).get("effort"),
    )
    for request in matching
)
expected = sorted(tuple(route) for route in json.loads(expected_json))
assert actual == expected, (marker, actual, expected)
PY
}

request_identity() {
  local marker="$1"
  python3 - "$request_log" "$marker" <<'PY'
import json
import sys

path, marker = sys.argv[1:]
for line in open(path, encoding="utf-8"):
    request = json.loads(line)
    if marker not in request.get("markers", []):
        continue
    thread_id = request.get("client_metadata", {}).get("thread_id")
    turn_id = request.get("client_metadata", {}).get("turn_id")
    if (
        isinstance(thread_id, str)
        and thread_id
        and isinstance(turn_id, str)
        and turn_id
        and turn_id != "session-name"
    ):
        print(request["sequence"], thread_id, turn_id)
        break
else:
    raise SystemExit(f"no request identity contains {marker!r}")
PY
}

wait_for_child_request_marker() {
  local marker="$1"
  local root_thread_id="$2"
  local root_sequence="$3"
  for _ in $(seq 1 600); do
    if python3 - "$request_log" "$marker" "$root_thread_id" "$root_sequence" <<'PY'
import json
import sys

path, marker, root_thread_id, root_sequence = sys.argv[1:]
for line in open(path, encoding="utf-8"):
    request = json.loads(line)
    if (
        int(request.get("sequence", 0)) > int(root_sequence)
        and marker in request.get("markers", [])
        and request.get("client_metadata", {}).get("thread_id") != root_thread_id
        and request.get("client_metadata", {}).get("turn_id") != "session-name"
    ):
        raise SystemExit(0)
raise SystemExit(1)
PY
    then
      return
    fi
    sleep 0.1
  done
  fail "timed out waiting for child Responses request marker: $marker"
}

wait_for_request_marker() {
  local marker="$1"
  for _ in $(seq 1 300); do
    if rg -Fq -- "$marker" "$request_log"; then
      return
    fi
    sleep 0.1
  done
  fail "timed out waiting for Responses request marker: $marker"
}

request_count() {
  wc -l <"$request_log"
}

wait_for_request_count() {
  local expected="$1"
  for _ in $(seq 1 300); do
    if [[ "$(request_count)" -ge "$expected" ]]; then
      return
    fi
    sleep 0.1
  done
  fail "timed out waiting for Responses request count: $expected"
}

assert_latest_request_route() {
  local expected_model="$1"
  local expected_effort="$2"
  python3 - "$request_log" "$expected_model" "$expected_effort" <<'PY'
import json
import sys

path, expected_model, expected_effort = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
matching = [
    request
    for request in requests
    if request.get("client_metadata", {}).get("turn_id") != "session-name"
]
assert matching, "no routed Responses request"
request = matching[-1]
actual = (request.get("model"), (request.get("reasoning") or {}).get("effort"))
assert actual == (expected_model, expected_effort), actual
PY
}

start_mock() {
  python3 "$mock_server" --port-file "$mock_port_file" --request-log "$request_log" \
    --hold-response-file "$hold_response_file" \
    >"$artifact_dir/mock.stdout.log" 2>"$artifact_dir/mock.stderr.log" &
  mock_pid="$!"
  for _ in $(seq 1 100); do
    [[ -s "$mock_port_file" ]] && return
    sleep 0.05
  done
  fail "local Responses mock did not publish a port"
}

prepare_package() {
  mkdir -p "$package_dir/bin" "$package_dir/xedoc-resources/model-router" "$package_dir/xedoc-path"
  cp "$binary" "$package_dir/bin/xedoc"
  cp "$source_router" "$package_dir/xedoc-resources/model-router/reference-router"
  cp "$source_embedder" "$package_dir/xedoc-resources/model-router/reference-router-embedder.py"
  cp "$source_semantic_policy" "$package_dir/xedoc-resources/model-router/reference-router.semantic-policy.json"
  cp "$source_policy" "$package_dir/xedoc-resources/model-router/reference-router.policy.json"
  [[ ! -e "$policy_path" ]] || fail "router state unexpectedly exists before bootstrap"
  chmod +x \
    "$package_dir/bin/xedoc" \
    "$package_dir/xedoc-resources/model-router/reference-router" \
    "$package_dir/xedoc-resources/model-router/reference-router-embedder.py"
  cat >"$package_dir/xedoc-package.json" <<'JSON'
{
  "layoutVersion": 1,
  "version": "tmux-e2e",
  "target": "local",
  "variant": "xedoc",
  "entrypoint": "bin/xedoc",
  "resourcesDir": "xedoc-resources",
  "modelRouterScript": "xedoc-resources/model-router/reference-router",
  "modelRouterPolicy": "xedoc-resources/model-router/reference-router.policy.json",
  "pathDir": "xedoc-path"
}
JSON
}

seed_legacy_policy() {
  local legacy_policy_path="$runtime_home/packages/standalone/releases/legacy/xedoc-resources/model-router/reference-router.policy.json"
  local invalid_legacy_policy_path="$runtime_home/packages/standalone/releases/corrupt/xedoc-resources/model-router/reference-router.policy.json"
  mkdir -p "$(dirname "$legacy_policy_path")"
  mkdir -p "$(dirname "$invalid_legacy_policy_path")"
  python3 - "$source_policy" "$legacy_policy_path" "$invalid_legacy_policy_path" <<'PY'
import json
import os
import sys
import time

source_path, destination_path, invalid_destination_path = sys.argv[1:]
policy = json.load(open(source_path, encoding="utf-8"))
policy["reportingBaseline"] = {
    "providerId": "openai",
    "model": "gpt-5.6-terra",
    "reasoningEffort": "high",
}
with open(destination_path, "w", encoding="utf-8") as output:
    json.dump(policy, output)
with open(invalid_destination_path, "w", encoding="utf-8") as output:
    json.dump({"semanticClassifier": None}, output)
now = time.time()
os.utime(destination_path, (now - 1, now - 1))
os.utime(invalid_destination_path, (now, now))
PY
}

assert_legacy_policy_migration() {
  start_tui
  open_settings
  assert_policy "reportingBaseline.providerId=openai"
  assert_policy "reportingBaseline.model=gpt-5.6-terra"
  assert_policy "reportingBaseline.reasoningEffort=high"
  reset_policy
  record_scenario legacy-policy-migration \
    "first TUI launch skips a newer corrupt release policy and migrates the prior release policy into XEDOC_HOME state"
}

write_runtime_config() {
  local port client_version
  port="$(<"$mock_port_file")"
  client_version="$("$binary" --version | awk 'NR == 1 { print $2 }')"
  [[ "$client_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail "could not determine xedoc client version for model cache: $client_version"
  mkdir -p "$runtime_home"
  python3 - "$source_catalog" "$runtime_home/models_cache_v2.json" "$client_version" <<'PY'
import datetime
import json
import pathlib
import sys

catalog_path = pathlib.Path(sys.argv[1])
cache_path = pathlib.Path(sys.argv[2])
client_version = sys.argv[3]
catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
cache_path.parent.mkdir(parents=True, exist_ok=True)
cache_path.write_text(
    json.dumps(
        {
            "fetched_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "etag": None,
            "client_version": client_version,
            "models": catalog["models"],
        }
    ),
    encoding="utf-8",
)
PY
  cat >"$runtime_home/auth.json" <<'JSON'
{
  "auth_mode": "apikey",
  "OPENAI_API_KEY": "router-e2e-key"
}
JSON
  cat >"$runtime_home/config.toml" <<EOF
model = "$initial_model"
model_provider = "openai"
model_reasoning_effort = "$initial_effort"
openai_base_url = "http://127.0.0.1:$port/v1"
model_catalog_json = "$source_catalog"
suppress_unstable_features_warning = true
sandbox_mode = "read-only"

[model_router]
decision_timeout_ms = 3000
interaction_timeout_ms = 10000

[model_providers.openai.model_prices."gpt-5.6-luna"]
input_price_per_1m_tokens = 1.0
cached_input_price_per_1m_tokens = 0.5
output_price_per_1m_tokens = 2.0

[model_providers.openai.model_prices."gpt-5.6-terra"]
input_price_per_1m_tokens = 2.0
cached_input_price_per_1m_tokens = 1.0
output_price_per_1m_tokens = 4.0

[model_providers.openai.model_prices."gpt-5.6-sol"]
input_price_per_1m_tokens = 3.0
cached_input_price_per_1m_tokens = 1.5
output_price_per_1m_tokens = 6.0
EOF
  record_config_digest
}

write_router_permission_hook() {
  local behavior="$1"
  python3 - "$runtime_home" "$permission_hook_log" "$behavior" <<'PY'
import json
import pathlib
import sys

runtime_home = pathlib.Path(sys.argv[1])
log_path = pathlib.Path(sys.argv[2])
behavior = sys.argv[3]
script_path = runtime_home / "model_router_permission_hook.py"
script_path.write_text(
    f"""import json
from pathlib import Path
import sys

payload = json.load(sys.stdin)
with Path({str(log_path)!r}).open("a", encoding="utf-8") as handle:
    handle.write(json.dumps(payload) + "\\n")

print(json.dumps({{
    "hookSpecificOutput": {{
        "hookEventName": "PermissionRequest",
        "decision": {{"behavior": {behavior!r}}},
    }}
}}))
""",
    encoding="utf-8",
)
hooks_path = runtime_home / "hooks.json"
hooks_path.write_text(
    json.dumps(
        {
            "hooks": {
                "PermissionRequest": [
                    {
                        "matcher": "model_router",
                        "hooks": [
                            {
                                "type": "command",
                                "command": f"python3 {script_path}",
                                "timeout_sec": 5,
                            }
                        ],
                    }
                ]
            }
        }
    ),
    encoding="utf-8",
)
PY
}

assert_router_permission_hook() {
  local expected_count="$1"
  python3 - "$permission_hook_log" "$expected_count" <<'PY'
import json
import pathlib
import sys

log_path = pathlib.Path(sys.argv[1])
expected_count = int(sys.argv[2])
entries = [
    json.loads(line)
    for line in log_path.read_text(encoding="utf-8").splitlines()
    if line
]
assert len(entries) == expected_count, entries
for entry in entries:
    assert entry["tool_name"] == "model_router", entry
    assert entry["tool_input"]["kind"] == "route_approval", entry
    assert entry["tool_input"]["surface"]["type"] == "confirmation", entry
PY
}

assert_standalone_launch() {
  local pane_start
  pane_start="$(tmux list-panes -t "$tmux_session":0 -F '#{pane_start_command}')"
  [[ "$pane_start" != *"--remote"* ]] || fail "standalone harness launched with --remote"
  [[ "$pane_start" != *"app-server"* ]] || fail "standalone harness launched app-server"
  [[ "$pane_start" == *"--no-alt-screen"* ]] || fail "standalone harness omitted --no-alt-screen"
  [[ "$pane_start" == *"$package_dir/bin/xedoc"* ]] || fail "standalone harness did not run packaged xedoc"
  local pane_pid
  pane_pid="$(tmux list-panes -t "$tmux_session":0 -F '#{pane_pid}')"
  local pane_command
  pane_command="$(ps -p "$pane_pid" -o command=)"
  [[ "$pane_command" != *"app-server"* ]] || fail "standalone harness pane runs app-server"
  [[ "$pane_command" == *"$package_dir/bin/xedoc"* ]] ||
    fail "standalone harness pane does not run packaged xedoc"
}

start_tui() {
  local router_feature="${1:-enabled}"
  if [[ -n "$tmux_session" ]]; then
    tmux kill-session -t "$tmux_session" >/dev/null 2>&1 || true
  fi
  tmux_session="xedoc-router-e2e-$RANDOM-$$"
  local args=(
    env
    "XEDOC_HOME=$runtime_home"
    "XEDOC_ROUTER_RUNTIME=$router_runtime"
    OPENAI_API_KEY=router-e2e-key
    XEDOC_DISABLE_AUTO_UPDATE=1
    "$package_dir/bin/xedoc"
  )
  if [[ "$router_feature" == "enabled" ]]; then
    args+=(--enable model_router)
  elif [[ "$router_feature" != "disabled" ]]; then
    fail "unknown router feature state: $router_feature"
  fi
  args+=(--enable multi_agent_v2 --no-alt-screen)
  local command
  printf -v command '%q ' "${args[@]}"
  local launcher
  printf -v launcher 'tmux set-option -t %q remain-on-exit on; exec %s' \
    "$tmux_session:0" "$command"
  tmux new-session -d -x 240 -y 65 -s "$tmux_session" "$launcher"
  wait_for_tui_ready
  record_config_digest
  assert_standalone_launch
}

open_settings() {
  [[ -n "$tmux_session" ]] || fail "cannot open settings before starting the isolated TUI"
  local pane
  pane="$(capture_viewport)"
  if [[ "$pane" == *"Model Router Settings"* && "$pane" == *"Press enter to confirm"* ]]; then
    send_key Escape
    wait_for_pane_absent "Model Router Settings"
  fi
  send_prompt "/model-router"
  wait_for_pane "Model Router Settings"
}

select_menu_item() {
  local index="$1"
  local expected="$2"
  local count=0
  send_key Home
  while (( count < index )); do
    send_key Down
    count=$((count + 1))
  done
  send_key Enter
  wait_for_pane "$expected"
}

set_select_value() {
  local expected="$1"
  local options="$2"
  local option="${expected#*: }"
  local pane
  send_key Right
  for _ in $(seq 0 "$options"); do
    pane="$(capture_viewport)"
    if [[ "$pane" == *"> $option"* ]]; then
      send_key Enter
      return
    fi
    send_key Down
  done
  fail "could not select form value: $expected"
}

set_baseline_terra_high() {
  set_select_value "Reporting baseline: openai/gpt-5.6-terra/high" 15
}

set_mode() {
  local mode="$1"
  local label="$2"
  open_settings
  select_menu_item 0 "Choose a setting, then apply your changes."
  set_select_value "Mode: $label" 5
  wait_for_pane "Mode: $mode"
  assert_policy "mode=$mode"
  assert_config_unchanged
}

set_approval() {
  local approval="$1"
  local label="$2"
  open_settings
  select_menu_item 2 "Choose a setting, then apply your changes."
  set_select_value "Approval prompts: $label" 3
  send_key Enter
  wait_for_pane "Approval prompts: $approval"
  assert_policy "approval=$approval"
  assert_config_unchanged
}

set_session_mode() {
  local mode="$1"
  local label="$2"
  open_settings
  select_menu_item 1 "Choose a setting for this session only."
  set_select_value "Session mode: $label" 6
  wait_for_pane "Session mode: $mode"
  assert_config_unchanged
}

set_feedback() {
  local expected="$1"
  local persisted="off"
  if [[ "$expected" == "true" ]]; then
    persisted="on"
  fi
  open_settings
  if [[ "$(capture_viewport)" != *"Routing feedback: $persisted"* ]]; then
    select_menu_item 3 "Routing feedback: $persisted"
  fi
  wait_for_pane "Routing feedback: $persisted"
  assert_policy "feedback=$expected"
  assert_config_unchanged
}

set_reporting_baseline() {
  open_settings
  select_menu_item 7 "Routing policy"
  select_menu_item 2 "Reporting baseline"
  set_baseline_terra_high
  wait_for_pane "Routing policy"
  assert_policy "reportingBaseline.providerId=openai"
  assert_policy "reportingBaseline.model=gpt-5.6-terra"
  assert_policy "reportingBaseline.reasoningEffort=high"
}

clear_reporting_baseline() {
  open_settings
  select_menu_item 7 "Routing policy"
  select_menu_item 2 "Reporting baseline"
  set_select_value "Reporting baseline: Not set" 16
  wait_for_pane "Routing policy"
  assert_policy "reportingBaseline=null"
}

assert_classifier_model_picker() {
  open_settings
  select_menu_item 7 "Routing policy"
  select_menu_item 3 "Classifier model"
  wait_for_pane "openai/gpt-5.6-luna/low"
}

exercise_host_action() {
  local index="$1"
  local name="$2"
  local before
  before="$(python3 - "$policy_path" <<'PY'
import json
import sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["revision"])
PY
)"
  open_settings
  local count=0
  while (( count < index )); do
    send_key Down
    count=$((count + 1))
  done
  send_key Enter
  if [[ "$name" == "open report" ]]; then
    wait_for_pane "Opened http://"
  else
    wait_for_pane "Model-router A/B setting updated."
  fi
  local after
  after="$(python3 - "$policy_path" <<'PY'
import json
import sys
print(json.load(open(sys.argv[1], encoding="utf-8"))["revision"])
PY
)"
  [[ "$before" == "$after" ]] || fail "$name unexpectedly rewrote script policy"
  assert_config_unchanged
}

assert_invocation_baseline() {
  local marker="$1"
  local expected_model="$2"
  local expected_effort="$3"
  local sequence
  local thread_id
  local turn_id
  read -r sequence thread_id turn_id <<<"$(request_identity "$marker")"
  for _ in $(seq 1 300); do
    if python3 - "$state_db" "$thread_id" "$turn_id" "$expected_model" "$expected_effort" <<'PY'
import sqlite3
import sys

path, thread_id, turn_id, expected_model, expected_effort = sys.argv[1:]
try:
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    row = connection.execute(
        """
        SELECT baseline_provider_id, baseline_model_slug,
               baseline_reasoning_effort, normalized_baseline_usd
        FROM model_router_invocations
        WHERE thread_id = ? AND turn_id = ? AND invocation_kind = 'regular'
        ORDER BY created_at DESC
        LIMIT 1
        """,
        (thread_id, turn_id),
    ).fetchone()
except sqlite3.Error:
    raise SystemExit(1)
if row is None:
    raise SystemExit(1)
if expected_model == "null":
    assert row == (None, None, None, None), row
else:
    assert row[:3] == ("openai", expected_model, expected_effort), row
    assert row[3] is not None and row[3] > 0, row
PY
    then
      return
    fi
    sleep 0.1
  done
  fail "timed out waiting for invocation baseline state: $marker"
}

assert_ab_pair_state() {
  local root_thread_id="$1"
  local root_turn_id="$2"
  for _ in $(seq 1 300); do
    if python3 - "$state_db" "$root_thread_id" "$root_turn_id" <<'PY'
import sqlite3
import sys

path, thread_id, turn_id = sys.argv[1:]
try:
    connection = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    outcome = connection.execute(
        """
        SELECT pair_id, outcome
        FROM model_router_ab_outcomes
        WHERE thread_id = ? AND turn_id = ?
        ORDER BY created_at DESC
        LIMIT 1
        """,
        (thread_id, turn_id),
    ).fetchone()
except sqlite3.Error:
    raise SystemExit(1)
if outcome is None:
    raise SystemExit(1)
pair_id, result = outcome
assert result == "pending", outcome
branches = connection.execute(
    """
    SELECT ab_branch, model_slug, reasoning_effort
    FROM model_router_invocations
    WHERE ab_pair_id = ?
    ORDER BY ab_branch
    """,
    (pair_id,),
).fetchall()
expected = {
    ("orchestrator", "gpt-5.6-luna", "low"),
    ("routed", "gpt-5.6-terra", "xhigh"),
}
assert set(branches) == expected, branches
assert all(sum(row[0] == branch for row in branches) >= 1 for branch, _, _ in expected)
PY
    then
      return
    fi
    sleep 0.1
  done
  fail "timed out waiting for persisted A/B pair state"
}

assert_subagent_decision_attribution() {
  for _ in $(seq 1 300); do
    if python3 - "$state_db" <<'PY'
import sqlite3
import sys

connection = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
total, ordinary, paired, routed, orchestrator = connection.execute(
    """
    SELECT
      COUNT(*),
      SUM(CASE WHEN i.ab_pair_id IS NULL THEN 1 ELSE 0 END),
      SUM(CASE WHEN i.ab_pair_id IS NOT NULL THEN 1 ELSE 0 END),
      SUM(CASE WHEN i.ab_branch = 'routed' THEN 1 ELSE 0 END),
      SUM(CASE WHEN i.ab_branch = 'orchestrator' THEN 1 ELSE 0 END)
    FROM model_router_invocations i
    JOIN model_router_decisions d ON d.decision_id = i.decision_id
    WHERE d.scope = 'subagent'
      AND i.turn_id != 'session-name'
      AND i.invocation_kind = 'regular'
    """
).fetchone()
assert (total, ordinary, paired, routed, orchestrator) == (9, 7, 2, 1, 1), (
    total,
    ordinary,
    paired,
    routed,
    orchestrator,
)
PY
    then
      record_scenario subagent-attribution \
        "all nine routed, retained, disabled-A/B, and paired child invocations attributed"
      return
    fi
    sleep 0.1
  done
  fail "timed out waiting for all scripted subagent invocations to be attributed"
}

open_report_and_assert() {
  require_command curl
  exercise_host_action 5 "open report"
  local report_pane="$artifact_dir/report-pane.log"
  local report_url_file="$artifact_dir/report-url.txt"
  local report_json="$artifact_dir/report.json"
  capture_viewport >"$report_pane"
  python3 - "$report_pane" "$report_url_file" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
urls = re.findall(r"Opened (http://[^\s]+#capability=[A-Za-z0-9-]+)", text)
assert urls, text
pathlib.Path(sys.argv[2]).write_text(urls[-1], encoding="utf-8")
PY
  local report_url
  report_url="$(<"$report_url_file")"
  local capability="${report_url##*#capability=}"
  local report_base="${report_url%%#*}"
  local api_url="${report_base%/}/api/report"
  curl --fail --silent --show-error \
    -H "X-Xedoc-Report-Capability: $capability" \
    "$api_url" >"$report_json"
  python3 - "$report_json" <<'PY'
import json
import sys

report = json.load(open(sys.argv[1], encoding="utf-8"))
assert report["days"], report
assert sum(day["invocations"] for day in report["days"]) > 0, report
assert sum(day["decisions"] for day in report["days"]) > 0, report
assert sum(day["attributedInvocations"] for day in report["days"]) > 0, report
assert report["recentDecisions"], report
PY
  record_scenario report \
    "capability-authenticated report returned decisions and attributed invocations"
}

establish_host_action_thread() {
  local expected_model="${1:-$initial_model}"
  local expected_effort="${2:-$initial_effort}"
  local marker="${3:-ROUTER_E2E_HOST_ACTION_THREAD}"
  rm -f "$hold_response_file"
  send_prompt "$marker ROUTER_E2E_HOLD_OPEN"
  wait_for_request_marker "$marker"
  assert_request_route "$marker" "$expected_model" "$expected_effort"
}

finish_host_action_thread() {
  local expected_model="${1:-$initial_model}"
  local expected_effort="${2:-$initial_effort}"
  local marker="${3:-ROUTER_E2E_HOST_ACTION_THREAD}"
  touch "$hold_response_file"
  await_turn
  assert_request_route "$marker" "$expected_model" "$expected_effort"
}

exercise_policy_manager() {
  open_settings
  select_menu_item 7 "Routing policy"

  send_key Home
  send_key Right
  wait_for_pane "Strict ("
  set_select_value "Confidence preset: Strict" 3
  wait_for_pane "Confidence: Strict"
  assert_policy "confidence=strict"

  send_key Home
  send_key Right
  wait_for_pane "Permissive ("
  set_select_value "Confidence preset: Permissive" 3
  wait_for_pane "Confidence: Permissive"
  assert_policy "confidence=permissive"

  send_key Home
  send_key Right
  wait_for_pane "Balanced ("
  set_select_value "Confidence preset: Balanced" 3
  wait_for_pane "Confidence: Balanced"
  assert_policy "confidence=balanced"
  record_scenario settings-confidence "strict, permissive, and balanced persisted"

  send_key Home
  send_key Down
  send_key Right
  wait_for_pane "Rank 1 ·"
  local rank
  for rank in $(seq 1 15); do
    local field_index=$((rank - 1))
    local count=0
    send_key Home
    while (( count < field_index )); do
      send_key Down
      count=$((count + 1))
    done
    send_key Right
    wait_for_pane "> openai/"
    set_select_value "Model route: openai/gpt-5.6-terra/high" 40
    wait_for_pane "Model ladder"
    assert_policy "ranking.ladder.$((rank - 1)).providerId=openai"
    assert_policy "ranking.ladder.$((rank - 1)).model=gpt-5.6-terra"
    assert_policy "ranking.ladder.$((rank - 1)).reasoningEffort=high"
  done
  record_scenario settings-ladder "all 15 ranked slots persisted through script forms"
  select_menu_item 15 "Routing policy"

  select_menu_item 2 "Reporting baseline"
  wait_for_pane "Reporting baseline:"
  set_baseline_terra_high
  wait_for_pane "Routing policy"
  assert_policy "reportingBaseline.providerId=openai"
  assert_policy "reportingBaseline.model=gpt-5.6-terra"
  assert_policy "reportingBaseline.reasoningEffort=high"

  select_menu_item 4 "Routing policy"
  assert_policy "reportingBaseline=null"
  record_scenario settings-baseline "baseline persisted and clear returned it to Not set"

  select_menu_item 5 "Model router"
  assert_config_unchanged
}

exercise_settings_matrix() {
  set_mode off Off
  set_mode shadow-subagents "Shadow Subagents"
  set_mode shadow-full "Shadow Full"
  set_mode subagents Subagents
  set_mode full Full

  set_approval off Off
  set_approval policy "By policy (not confident)"
  set_approval all "All available routes"

  set_feedback false
  set_feedback true

  exercise_policy_manager
  record_scenario settings-root-menu \
    "all modes, approvals, feedback, policy, ladder, baseline, and back items exercised"
}

await_turn() {
  wait_for_pane "router root completed" 500
}

run_root_mode() {
  local mode="$1"
  local marker="$2"
  local disposition="$3"
  set_mode "$mode" "${4:-$mode}"
  set_approval off Off
  start_tui
  send_prompt "$marker review workflow security"
  wait_for_request_marker "$marker"
  await_turn
  case "$disposition" in
    applied)
      assert_request_route "$marker" "gpt-5.6-sol" high
      wait_for_pane "model router Root Applied: openai/gpt-5.6-sol/high"
      ;;
    shadow)
      assert_request_route "$marker" "$initial_model" "$initial_effort"
      wait_for_pane \
        "model router Root Shadow: openai/gpt-5.6-sol/high proposed; kept openai/gpt-5.6-luna/low"
      ;;
    retained)
      assert_request_route "$marker" "$initial_model" "$initial_effort"
      ;;
    *)
      fail "unknown expected root disposition: $disposition"
      ;;
  esac
  record_scenario "mode-root-$mode" "$disposition route verified from Responses request"
}

run_spawn_mode() {
  local mode="$1"
  local expected_child_route="$2"
  local marker="ROUTER_E2E_SPAWN_ROOT_${mode}"
  local root_sequence
  local root_thread_id
  local root_turn_id
  set_mode "$mode" "$3"
  set_approval off Off
  start_tui
  send_prompt "$marker review workflow security"
  wait_for_request_marker "$marker"
  read -r root_sequence root_thread_id root_turn_id <<<"$(request_identity "$marker")"
  wait_for_pane "router parent completed" 600
  wait_for_child_request_marker "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence"
  assert_child_request_count "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence" 1
  if [[ "$expected_child_route" == "routed" ]]; then
    assert_request_route \
      "ROUTER_E2E_CHILD" "gpt-5.6-terra" xhigh "$root_thread_id" "$root_sequence"
  else
    assert_request_route "ROUTER_E2E_CHILD" "$initial_model" "$initial_effort" "$root_thread_id" "$root_sequence"
  fi
  record_scenario "mode-subagent-$mode" "$expected_child_route child route verified"
}

run_mode_matrix() {
  reset_policy
  start_tui
  run_root_mode off ROUTER_E2E_MODE_OFF retained Off
  run_spawn_mode off original Off
  run_spawn_mode shadow-subagents original "Shadow Subagents"
  run_root_mode shadow-subagents ROUTER_E2E_MODE_SHADOW_SUBAGENTS_ROOT retained \
    "Shadow Subagents"
  run_root_mode shadow-full ROUTER_E2E_MODE_SHADOW_FULL shadow "Shadow Full"
  run_spawn_mode shadow-full original "Shadow Full"
  run_spawn_mode subagents routed Subagents
  run_root_mode subagents ROUTER_E2E_MODE_SUBAGENTS_ROOT retained Subagents
  run_spawn_mode full routed Full
  run_root_mode full ROUTER_E2E_MODE_FULL applied Full
}

run_classifier_mode_matrix() {
  local root_sequence
  local root_thread_id
  reset_policy
  start_tui
  assert_classifier_model_picker
  set_classifier_route full
  start_tui
  send_prompt "ROUTER_E2E_CLASSIFIER_FULL review workflow security"
  wait_for_request_marker "ROUTER_E2E_CLASSIFIER_FULL"
  assert_classifier_requests "ROUTER_E2E_CLASSIFIER_FULL" 1
  await_turn
  assert_latest_hybrid_decision \
    root full \
    "group1: question, docs_analysis, packaging, operational, testing" \
    high low none apply

  set_classifier_route shadow-full
  start_tui
  send_prompt "ROUTER_E2E_CLASSIFIER_SHADOW review workflow security"
  wait_for_request_marker "ROUTER_E2E_CLASSIFIER_SHADOW"
  assert_classifier_requests "ROUTER_E2E_CLASSIFIER_SHADOW" 1
  await_turn
  assert_request_route "ROUTER_E2E_CLASSIFIER_SHADOW" "$initial_model" "$initial_effort"
  assert_latest_hybrid_decision \
    root shadow-full \
    "group1: question, docs_analysis, packaging, operational, testing" \
    very_high medium delegate shadow

  set_classifier_route subagents
  start_tui
  send_prompt "ROUTER_E2E_CLASSIFIER_SUBAGENT ROUTER_E2E_SPAWN_ROOT_classifier review workflow security"
  wait_for_request_marker "ROUTER_E2E_CLASSIFIER_SUBAGENT"
  read -r root_sequence root_thread_id _ <<<"$(
    request_identity "ROUTER_E2E_CLASSIFIER_SUBAGENT"
  )"
  wait_for_pane "router parent completed" 600
  wait_for_child_request_marker "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence"
  assert_classifier_requests "ROUTER_E2E_CLASSIFIER_SUBAGENT" 2
  assert_classifier_requests "ROUTER_E2E_CHILD" 1
  assert_latest_hybrid_decision \
    subagent subagents \
    "group3: research, review, diagnosis, design" \
    low low none apply
  record_scenario classifier-modes \
    "LLM classification and embedding work-type similarity reached full, shadow, and subagent routing"
}

run_session_mode_override() {
  reset_policy
  start_tui
  set_mode off Off
  set_session_mode full Full
  assert_policy "mode=off"
  set_approval all "All available routes"
  send_key Escape
  send_prompt "ROUTER_E2E_SESSION_OVERRIDE review workflow security"
  assert_approval_details
  send_key Enter
  wait_for_request_marker "ROUTER_E2E_SESSION_OVERRIDE"
  await_turn
  assert_request_route "ROUTER_E2E_SESSION_OVERRIDE" "gpt-5.6-terra" low
  assert_policy "mode=off"
  record_scenario session-mode-override \
    "session mode full overrode shared off mode and required approval"
}

run_steering_matrix() {
  local seeded_model="gpt-5.6-terra"
  local seeded_effort="low"
  reset_policy
  set_mode full Full
  set_approval off Off
  set_feedback true
  start_tui

  send_prompt "ROUTER_E2E_STEERING_SEED what are the current security review options"
  wait_for_request_marker "ROUTER_E2E_STEERING_SEED"
  await_turn
  assert_request_route "ROUTER_E2E_STEERING_SEED" "$seeded_model" "$seeded_effort"
  assert_latest_router_decision false no

  local expected_requests
  expected_requests=$(( $(request_count) + 1 ))
  send_prompt "both"
  wait_for_request_count "$expected_requests"
  await_turn
  assert_latest_request_route "$initial_model" "$initial_effort"
  assert_latest_router_decision true yes

  expected_requests=$(( $(request_count) + 1 ))
  send_prompt \
    "Please continue from the options above, choose the recommended one, and carry out the implementation in the active task."
  wait_for_request_count "$expected_requests"
  await_turn
  assert_latest_request_route "$initial_model" "$initial_effort"
  assert_latest_router_decision true yes
  record_scenario steering-followups \
    "completed-turn token and long semantic follow-ups retained the current route"
}

run_feature_disabled() {
  reset_policy
  start_tui
  set_mode full Full
  set_approval off Off
  set_feedback true
  start_tui disabled
  send_prompt "ROUTER_E2E_FEATURE_DISABLED review workflow security"
  wait_for_request_marker "ROUTER_E2E_FEATURE_DISABLED"
  await_turn
  assert_request_route \
    "ROUTER_E2E_FEATURE_DISABLED" "$initial_model" "$initial_effort"
  wait_for_pane_absent "model router Root"
  assert_policy "mode=full"
  record_scenario feature-disabled \
    "packaged sibling policy stayed full while host omitted scripted routing"
}

assert_approval_details() {
  wait_for_pane "Routing:"
  wait_for_pane "Prompt:"
  wait_for_pane "Classification"
  wait_for_pane "Confidence"
}

run_approval_matrix() {
  local root_sequence
  local root_thread_id
  local root_turn_id
  reset_policy
  set_mode full Full
  set_feedback true

  start_tui
  send_prompt "ROUTER_CASE_CHANGES_SAME"
  assert_approval_details
  send_key Enter
  wait_for_request_marker "ROUTER_CASE_CHANGES_SAME"
  await_turn
  assert_request_route "ROUTER_CASE_CHANGES_SAME" "gpt-5.6-terra" low
  record_scenario approval-policy-uncalibrated-same \
    "uncalibrated unchanged route required confirmation"

  start_tui
  send_prompt "ROUTER_E2E_CHANGES_CHANGED review workflow security"
  assert_approval_details
  send_key Enter
  wait_for_request_marker "ROUTER_E2E_CHANGES_CHANGED"
  await_turn
  assert_request_route "ROUTER_E2E_CHANGES_CHANGED" "gpt-5.6-terra" low
  record_scenario approval-policy-uncalibrated-changed \
    "uncalibrated changed route required and accepted confirmation"

  set_approval all "All available routes"
  start_tui

  send_prompt "ROUTER_CASE_ALL_SAME"
  assert_approval_details
  send_key Enter
  wait_for_request_marker "ROUTER_CASE_ALL_SAME"
  await_turn
  assert_request_route "ROUTER_CASE_ALL_SAME" "gpt-5.6-terra" low
  record_scenario approval-all-same "unchanged route still required confirmation"

  send_prompt "ROUTER_E2E_REJECT review workflow security"
  assert_approval_details
  send_key Escape
  wait_for_request_marker "ROUTER_E2E_REJECT"
  await_turn
  assert_request_route "ROUTER_E2E_REJECT" "$initial_model" "$initial_effort"
  record_scenario approval-reject "changed route rejected and current route retained"

  send_prompt "ROUTER_E2E_SUBAGENT_APPROVAL ROUTER_E2E_SPAWN_ROOT_approval review workflow security"
  assert_approval_details
  send_key Enter
  wait_for_request_marker "ROUTER_E2E_SUBAGENT_APPROVAL"
  read -r root_sequence root_thread_id root_turn_id <<<"$(
    request_identity "ROUTER_E2E_SUBAGENT_APPROVAL"
  )"
  wait_for_pane "group1: question, docs_analysis" 600
  wait_for_pane "Use openai/" 600
  assert_approval_details
  send_key Escape
  wait_for_pane "router parent completed" 600
  wait_for_child_request_marker "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence"
  assert_request_route \
    "ROUTER_E2E_CHILD" "gpt-5.6-sol" low "$root_thread_id" "$root_sequence"
  record_scenario approval-subagent-reject \
    "subagent confirmation rejected and current child route retained"
}

run_router_permission_hook_matrix() {
  reset_policy
  start_tui
  set_mode full Full
  set_approval all "All available routes"
  set_feedback true

  : >"$permission_hook_log"
  write_router_permission_hook allow
  start_tui
  send_prompt "ROUTER_E2E_PERMISSION_HOOK_ALLOW review workflow security"
  wait_for_request_marker "ROUTER_E2E_PERMISSION_HOOK_ALLOW"
  await_turn
  assert_request_route \
    "ROUTER_E2E_PERMISSION_HOOK_ALLOW" "gpt-5.6-terra" low
  assert_router_permission_hook 1
  record_scenario permission-hook-allow \
    "PermissionRequest hook accepted the model-router route without rendering confirmation"

  : >"$permission_hook_log"
  write_router_permission_hook deny
  start_tui
  send_prompt "ROUTER_E2E_PERMISSION_HOOK_DENY review workflow security"
  wait_for_request_marker "ROUTER_E2E_PERMISSION_HOOK_DENY"
  await_turn
  assert_request_route \
    "ROUTER_E2E_PERMISSION_HOOK_DENY" "$initial_model" "$initial_effort"
  assert_router_permission_hook 1
  record_scenario permission-hook-deny \
    "PermissionRequest hook retained the current route without rendering confirmation"
}

run_override_case() {
  local axis="$1"
  local field_index="$2"
  local expected_form="$3"
  local expected_classification="$4"
  local expected_calculation="$5"
  local expected_model="$6"
  local expected_effort="$7"
  local marker="ROUTER_CASE_OVERRIDE_${axis}"
  start_tui
  send_prompt "$marker"
  assert_approval_details
  wait_for_pane "Change classification [o]"
  send_key o
  wait_for_pane "Change classification"
  local count=0
  while (( count < field_index )); do
    send_key Down
    count=$((count + 1))
  done
  send_key Right
  set_select_value "$expected_form" 8
  wait_for_pane "Use openai/"
  wait_for_pane "$expected_classification"
  wait_for_pane "similarity 100% / margin 100%"
  wait_for_pane "$expected_calculation"
  wait_for_pane "openai/$expected_model/$expected_effort"
  send_key Enter
  wait_for_request_marker "$marker"
  await_turn
  assert_request_route "$marker" "$expected_model" "$expected_effort"
  record_scenario "override-$axis" \
    "reclassified, recomputed, approved, and applied $expected_model/$expected_effort"
}

run_override_matrix() {
  reset_policy
  set_mode full Full
  set_approval all "All available routes"
  set_feedback true
  run_override_case work_type 0 "Work type: Group2:" \
    "group2: implementation" "score 5 (bounded 5;" "gpt-5.6-luna" medium
  run_override_case complexity 1 "Complexity: Medium" \
    "medium complexity" "score 5 (bounded 5;" "gpt-5.6-luna" medium
  run_override_case orchestration 2 "Orchestration: Coordination" \
    "coordination orchestration" "score 4 (bounded 4;" "gpt-5.6-terra" low
  run_override_case risk 3 "Risk: Medium" \
    "medium risk" "score 5 (bounded 5;" "gpt-5.6-luna" medium
}

run_feedback_matrix() {
  reset_policy
  set_mode full Full
  set_approval off Off
  set_feedback false
  start_tui
  send_prompt "ROUTER_E2E_FEEDBACK_OFF review workflow security"
  wait_for_request_marker "ROUTER_E2E_FEEDBACK_OFF"
  await_turn
  wait_for_pane_absent "model router Root"
  assert_request_route "ROUTER_E2E_FEEDBACK_OFF" "gpt-5.6-sol" high
  record_scenario feedback-off "script route applied with no visible feedback"

  set_feedback true
  start_tui
  send_prompt "ROUTER_E2E_FEEDBACK_ON review workflow security"
  wait_for_request_marker "ROUTER_E2E_FEEDBACK_ON"
  await_turn
  wait_for_pane "model router Root Applied: openai/gpt-5.6-sol/high"
  wait_for_pane \
    "classification=group3: research, review, diagnosis, design; very_high complexity;"
  wait_for_pane "workflow orchestration; high risk"
  wait_for_pane "routing calculation=score 28 (bounded 28; domain 3–35)"
  wait_for_pane "classes smart–intelligent"
  wait_for_pane "ranks 6–15"
  wait_for_pane "target 13"
  wait_for_pane "selected 13"
  wait_for_pane "confidence 0.86/0.56 (minimum 0.35/0.08)"
  assert_request_route "ROUTER_E2E_FEEDBACK_ON" "gpt-5.6-sol" high
  record_scenario feedback-on \
    "classification, confidence, ranking calculation, and exact model choice visible"
}

run_shadow_feedback_matrix() {
  reset_policy
  set_mode shadow-full "Shadow Full"
  set_approval off Off
  set_feedback true
  start_tui
  send_prompt "ROUTER_E2E_SHADOW_FEEDBACK review workflow security"
  wait_for_request_marker "ROUTER_E2E_SHADOW_FEEDBACK"
  await_turn
  assert_request_route \
    "ROUTER_E2E_SHADOW_FEEDBACK" "$initial_model" "$initial_effort"
  wait_for_pane "Decision: shadow; Target model: openai/"
  wait_for_pane "Used model: openai/$initial_model/$initial_effort;"
  wait_for_pane "Classifications: Work type("
  wait_for_pane "Stats: Embedding("
  local pane
  pane="$(capture_viewport)"
  [[ "$pane" == *$'model router\n  └ Decision: shadow; Target model: openai/'* ]] ||
    fail "shadow feedback did not render the router heading separately"
  wait_for_pane_absent "embedding batch "
  record_scenario shadow-feedback \
    "script-owned shadow summary visible without duplicate routing calculation"
}

run_baseline_report_matrix() {
  reset_policy
  start_tui
  set_mode full Full
  set_approval off Off
  set_feedback true
  set_reporting_baseline

  start_tui
  send_prompt "ROUTER_E2E_BASELINE_SET review workflow security"
  wait_for_request_marker "ROUTER_E2E_BASELINE_SET"
  await_turn
  assert_request_route "ROUTER_E2E_BASELINE_SET" "gpt-5.6-terra" medium
  assert_invocation_baseline \
    "ROUTER_E2E_BASELINE_SET" "gpt-5.6-terra" high
  record_scenario baseline-set \
    "script baseline reached persisted invocation cost attribution"
  open_report_and_assert

  clear_reporting_baseline
  start_tui
  send_prompt "ROUTER_E2E_BASELINE_CLEAR review workflow security"
  wait_for_request_marker "ROUTER_E2E_BASELINE_CLEAR"
  await_turn
  assert_request_route "ROUTER_E2E_BASELINE_CLEAR" "gpt-5.6-terra" medium
  assert_invocation_baseline "ROUTER_E2E_BASELINE_CLEAR" null null
  record_scenario baseline-clear \
    "cleared baseline reached persisted invocation as Not set"
}

run_ab_matrix() {
  reset_policy
  start_tui
  set_mode full Full
  set_approval off Off
  set_feedback true

  start_tui
  establish_host_action_thread \
    "gpt-5.6-terra" xhigh ROUTER_E2E_HOST_ACTION_THREAD_AB_DISABLE
  exercise_host_action 3 "arm A/B"
  exercise_host_action 4 "disable A/B"
  finish_host_action_thread \
    "gpt-5.6-terra" xhigh ROUTER_E2E_HOST_ACTION_THREAD_AB_DISABLE

  local marker="ROUTER_E2E_SPAWN_ROOT_ab_disabled"
  local root_sequence
  local root_thread_id
  local root_turn_id
  send_prompt "$marker review workflow security"
  wait_for_request_marker "$marker"
  read -r root_sequence root_thread_id root_turn_id <<<"$(request_identity "$marker")"
  wait_for_pane "router parent completed" 600
  wait_for_child_request_marker "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence"
  sleep 1
  assert_exact_child_routes \
    "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence" \
    '[["gpt-5.6-terra","xhigh"]]'
  record_scenario ab-disable \
    "arm then disable produced one routed child instead of an A/B pair"

  start_tui
  establish_host_action_thread \
    "gpt-5.6-terra" xhigh ROUTER_E2E_HOST_ACTION_THREAD_AB_PAIR
  exercise_host_action 3 "arm A/B"
  finish_host_action_thread \
    "gpt-5.6-terra" xhigh ROUTER_E2E_HOST_ACTION_THREAD_AB_PAIR

  marker="ROUTER_E2E_SPAWN_ROOT_ab_pair"
  send_prompt "$marker review workflow security"
  wait_for_request_marker "$marker"
  read -r root_sequence root_thread_id root_turn_id <<<"$(request_identity "$marker")"
  wait_for_pane "router parent completed" 600
  wait_for_child_request_marker "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence"
  for _ in $(seq 1 300); do
    if python3 - "$request_log" "$root_thread_id" "$root_sequence" <<'PY'
import json
import sys

path, root_thread_id, root_sequence = sys.argv[1:]
requests = [json.loads(line) for line in open(path, encoding="utf-8") if line.strip()]
count = sum(
    int(request.get("sequence", 0)) > int(root_sequence)
    and "ROUTER_E2E_CHILD" in request.get("markers", [])
    and request.get("client_metadata", {}).get("thread_id") != root_thread_id
    and request.get("client_metadata", {}).get("turn_id") != "session-name"
    for request in requests
)
raise SystemExit(0 if count >= 2 else 1)
PY
    then
      break
    fi
    sleep 0.1
  done
  assert_exact_child_routes \
    "ROUTER_E2E_CHILD" "$root_thread_id" "$root_sequence" \
    '[["gpt-5.6-luna","low"],["gpt-5.6-terra","xhigh"]]'
  assert_ab_pair_state "$root_thread_id" "$root_turn_id"
  record_scenario ab-pair \
    "arm produced exact routed and orchestrator branches plus persisted pair state"
}

assert_bounded_artifacts() {
  python3 - "$request_log" "$scenario_log" "$artifact_dir" <<'PY'
import json
import pathlib
import sys

request_path = pathlib.Path(sys.argv[1])
scenario_path = pathlib.Path(sys.argv[2])
artifact_dir = pathlib.Path(sys.argv[3])
lines = request_path.read_text(encoding="utf-8").splitlines()
assert 0 < len(lines) <= 300, len(lines)
allowed = {
    "sequence",
    "path",
    "model",
    "reasoning",
    "request_kind",
    "client_metadata",
    "markers",
}
for line in lines:
    assert len(line.encode()) <= 4096, len(line.encode())
    observation = json.loads(line)
    assert set(observation) == allowed, observation
    assert len(observation["markers"]) <= 16, observation
scenarios = scenario_path.read_text(encoding="utf-8").splitlines()
assert len(scenarios) >= 30, len(scenarios)
for name in ("mock.stdout.log", "mock.stderr.log"):
    assert (artifact_dir / name).stat().st_size <= 65536, name
PY
}

main() {
  require_command tmux
  require_command python3
  require_command shasum
  [[ -x "$binary" ]] || fail "XEDOC_TMUX_TEST_BIN is not executable: $binary"
  [[ -x "$source_router" ]] || fail "missing packaged reference router: $source_router"
  [[ -f "$source_policy" ]] || fail "missing packaged reference policy: $source_policy"
  [[ -x "$source_embedder" ]] || fail "missing packaged semantic embedder: $source_embedder"
  [[ -f "$source_semantic_policy" ]] || fail "missing packaged semantic policy: $source_semantic_policy"
  [[ -f "$source_catalog" ]] || fail "missing local model catalog: $source_catalog"
  [[ -f "$mock_server" ]] || fail "missing local Responses mock: $mock_server"
  [[ -n "$router_runtime" ]] || fail "XEDOC_TMUX_ROUTER_RUNTIME must name an installed semantic runtime"
  [[ -x "$(runtime_python)" ]] || fail "semantic runtime Python is missing"
  mkdir -p "$artifact_dir"
  : >"$request_log"
  tmux start-server
  start_mock
  prepare_package
  write_runtime_config
  seed_legacy_policy
  assert_reference_policy_contract
  assert_legacy_policy_migration
  assert_script_conflict_protocol

  local phase="${XEDOC_TMUX_TEST_PHASE:-full}"
  if [[ "$phase" == "full" ]]; then
    start_tui
    establish_host_action_thread
    exercise_settings_matrix
    finish_host_action_thread
    run_feature_disabled
    run_mode_matrix
    run_classifier_mode_matrix
    run_steering_matrix
  elif [[ "$phase" == "classifier" ]]; then
    run_classifier_mode_matrix
    assert_config_unchanged
    printf 'PASS: scripted model-router classifier tmux acceptance\n'
    return
  elif [[ "$phase" == "session-override" ]]; then
    run_session_mode_override
    assert_config_unchanged
    printf 'PASS: session-scoped scripted model-router tmux acceptance\n'
    return
  elif [[ "$phase" == "steering" ]]; then
    start_tui
    run_steering_matrix
    assert_config_unchanged
    printf 'PASS: scripted model-router tmux steering acceptance\n'
    return
  elif [[ "$phase" == "permission-hook" ]]; then
    run_router_permission_hook_matrix
    assert_config_unchanged
    printf 'PASS: scripted model-router permission-hook tmux acceptance\n'
    return
  elif [[ "$phase" == "shadow-feedback" ]]; then
    start_tui
    run_shadow_feedback_matrix
    assert_config_unchanged
    printf 'PASS: scripted model-router shadow-feedback tmux acceptance\n'
    return
  elif [[ "$phase" == "approval" ]]; then
    start_tui
    run_approval_matrix
    assert_config_unchanged
    printf 'PASS: scripted model-router approval tmux acceptance\n'
    return
  elif [[ "$phase" == "post-modes" ]]; then
    start_tui
  elif [[ "$phase" == "baseline-ab" || "$phase" == "ab" ]]; then
    start_tui
  else
    fail "unknown XEDOC_TMUX_TEST_PHASE: $phase"
  fi
  if [[ "$phase" != "baseline-ab" && "$phase" != "ab" ]]; then
    run_approval_matrix
    run_override_matrix
    run_feedback_matrix
  fi
  if [[ "$phase" != "ab" ]]; then
    run_baseline_report_matrix
  fi
  run_ab_matrix

  assert_config_unchanged
  if [[ "$phase" == "full" ]]; then
    assert_subagent_decision_attribution
    assert_bounded_artifacts
  fi
  printf 'PASS: standalone scripted model-router tmux acceptance matrix\n'
}

main "$@"
