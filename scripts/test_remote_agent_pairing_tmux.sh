#!/usr/bin/env bash
# Packaged remote-agent pairing acceptance harness.  It deliberately exercises
# the public enrollment/pairing CLIs and model tools; it never seeds trust state.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
python_bin="${XEDOC_REMOTE_AGENT_PAIRING_E2E_PYTHON:-python3}"
keep_artifacts="${XEDOC_REMOTE_AGENT_PAIRING_E2E_KEEP_ARTIFACTS:-0}"
package_root="${XEDOC_REMOTE_AGENT_PAIRING_E2E_PACKAGE:-}"

usage() {
  cat <<'USAGE'
Usage: scripts/test_remote_agent_pairing_tmux.sh [--package DIRECTORY]

Use a freshly built macOS ARM package directory containing bin/xedoc,
bin/xedoc-session, and bin/xedoc-remote-agentd.  Alternatively set
XEDOC_REMOTE_AGENT_PAIRING_E2E_PACKAGE.  Set
XEDOC_REMOTE_AGENT_PAIRING_E2E_KEEP_ARTIFACTS=1 to retain diagnostics.
USAGE
}
while [[ $# -gt 0 ]]; do
  case "$1" in
    --package) package_root="$2"; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    *) printf 'unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

case "$(uname -s)" in
  Darwin) ;;
  *) printf '%s\n' 'SKIP: pairing tmux E2E requires a packaged macOS ARM build.'; exit 0 ;;
esac
command -v tmux >/dev/null || { echo 'tmux is required' >&2; exit 1; }
command -v "$python_bin" >/dev/null || { echo "$python_bin is required" >&2; exit 1; }
[[ -n "$package_root" ]] || { echo 'set XEDOC_REMOTE_AGENT_PAIRING_E2E_PACKAGE or pass --package' >&2; exit 2; }
package_root="$($python_bin - "$package_root" <<'PY'
from pathlib import Path
import sys
print(Path(sys.argv[1]).resolve())
PY
)"
for binary in xedoc xedoc-session xedoc-remote-agentd; do
  [[ -x "$package_root/bin/$binary" ]] || { echo "missing package binary: $package_root/bin/$binary" >&2; exit 2; }
done

base="${XEDOC_REMOTE_AGENT_PAIRING_E2E_TMPDIR:-/private/tmp}"
mkdir -p "$base"
tmp_dir="$(mktemp -d "$base/xedoc-pairing.XXXXXX")"
tmp_dir="$(cd -- "$tmp_dir" && pwd -P)"
outside_dir="$tmp_dir/outside"
artifacts="$tmp_dir/artifacts"
mkdir -p "$outside_dir" "$artifacts"
chmod 700 "$outside_dir" "$artifacts"
package_xedoc="$package_root/bin/xedoc"
package_session="$package_root/bin/xedoc-session"
package_agent="$package_root/bin/xedoc-remote-agentd"
controller_timeout=30
current_session=""

fail() { echo "FAIL: $*" >&2; diagnostics; exit 1; }
diagnostics() {
  echo "--- pairing E2E artifacts: $tmp_dir ---" >&2
  if [[ -n "$current_session" ]]; then
    while IFS= read -r window; do
      tmux list-panes -t "$window" -F '#{pane_id} #{pane_title} dead=#{pane_dead}' >&2 2>/dev/null || true
    done < <(tmux list-windows -t "$current_session" -F '#{window_id}' 2>/dev/null || true)
    while IFS= read -r pane; do
      echo "--- $pane ---" >&2
      tmux capture-pane -p -t "$pane" -S -180 >&2 2>/dev/null || true
    done < <(
      while IFS= read -r window; do
        tmux list-panes -t "$window" -F '#{pane_id}' 2>/dev/null || true
      done < <(tmux list-windows -t "$current_session" -F '#{window_id}' 2>/dev/null || true)
    )
  fi
  find "$artifacts" -type f -maxdepth 2 -print0 2>/dev/null | while IFS= read -r -d '' path; do
    echo "--- ${path#"$artifacts"/} ---" >&2
    tail -n 120 "$path" >&2 || true
  done
}
cleanup() {
  local status="$?"
  [[ -n "$current_session" ]] && tmux kill-session -t "$current_session" >/dev/null 2>&1 || true
  if [[ "$status" -eq 0 && "$keep_artifacts" != 1 ]]; then rm -rf -- "$tmp_dir"; else echo "Artifacts retained at $tmp_dir" >&2; fi
}
trap cleanup EXIT INT TERM HUP

free_port() { "$python_bin" - <<'PY'
import socket
with socket.socket() as socket_:
    socket_.bind(("127.0.0.1", 0))
    print(socket_.getsockname()[1])
PY
}
wait_file() { local path="$1"; for _ in {1..600}; do [[ -s "$path" ]] && return; sleep .05; done; fail "timed out waiting for $path"; }
wait_socket() { "$python_bin" - "$1" <<'PY'
import socket, sys, time
for _ in range(600):
    client=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); client.settimeout(.1)
    try: client.connect(sys.argv[1]); raise SystemExit(0)
    except OSError: time.sleep(.05)
    finally: client.close()
raise SystemExit('timed out waiting for app server')
PY
}
wait_pane_exit() { local pane="$1"; for _ in {1..2400}; do [[ "$(tmux display-message -p -t "$pane" '#{pane_dead}' 2>/dev/null || true)" == 1 ]] && return; sleep .05; done; fail "timed out waiting for $pane"; }
wait_pane_text() { local pane="$1" text="$2"; for _ in {1..600}; do tmux capture-pane -p -t "$pane" -S -100 2>/dev/null | grep -Fq -- "$text" && return; sleep .05; done; fail "timed out waiting for $text"; }
menu_index_for_port() {
  local home="$1" port="$2" discovery_file="$3"
  env -u PYTHONPATH TMPDIR="$tmp_dir" "$package_agent" enrollment discover \
    --xedoc-home "$home" >"$discovery_file"
  "$python_bin" - "$discovery_file" "$port" <<'PY'
import json
from pathlib import Path
import sys

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
entries = value.get("data") if isinstance(value, dict) else None
if not isinstance(entries, list):
    raise SystemExit("discovery response omitted data")
hosts = sorted(
    (
        entry
        for entry in entries
        if isinstance(entry, dict)
        and entry.get("role") in {"coordinator", "managed"}
        and isinstance(entry.get("status"), str)
        and isinstance(entry.get("hostId"), str)
        and isinstance(entry.get("fingerprint"), str)
        and isinstance(entry.get("endpoint"), str)
    ),
    key=lambda entry: entry["hostId"],
)
port = sys.argv[2]
for index, entry in enumerate(hosts):
    if entry["endpoint"].rsplit(":", 1)[-1] == port:
        print(index)
        break
else:
    raise SystemExit(f"target peer on port {port} was not discovered")
PY
}

launcher() {
  local name="$1"; shift
  local file="$artifacts/$name.sh"
  { printf '#!/usr/bin/env bash\nset -euo pipefail\ncd -- %q\nunset PYTHONPATH\nexec ' "$outside_dir"; printf '%q ' "$@"; printf '\n'; } >"$file"
  chmod 700 "$file"; printf '%s\n' "$file"
}
new_pane() {
  local title="$1" command="$2" pane
  pane="$(tmux new-window -d -P -F '#{pane_id}' -t "$current_session" -n "$title" "$command")"
  tmux set-option -t "$pane" remain-on-exit on
  tmux select-pane -t "$pane" -T "$title"
  printf '%s\n' "$pane"
}
start_phase() { current_session="xedoc-pairing-$1-$$-$RANDOM"; tmux new-session -d -x 240 -y 64 -s "$current_session" 'exec bash'; tmux set-option -t "$current_session" remain-on-exit on; tmux select-pane -t "$current_session":0.0 -T shell; }
stop_phase() { tmux kill-session -t "$current_session" >/dev/null 2>&1 || true; current_session=""; }

write_config() {
  local home="$1" role="$2" workspace="$3" normal_port="$4" pairing_port="$5" mock_port="$6"
  mkdir -p "$home" "$workspace" "$home/remote-agent"
  chmod 700 "$home" "$workspace" "$home/remote-agent"
  cat >"$home/config.toml" <<EOF_CONFIG
model = "mock-model"
model_provider = "pairing_e2e"
approval_policy = "never"
sandbox_mode = "read-only"
auto_session_name = false

[model_providers.pairing_e2e]
name = "Pairing E2E mock"
base_url = "http://127.0.0.1:$mock_port/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
requires_openai_auth = false
supports_websockets = false
namespace_tools = true

[remote_agent]
role = "$role"
workspaces = { workspace_root = "$workspace" }
peer_listener = { endpoint = "tls://0.0.0.0:$normal_port", pairing_endpoint = "tls://0.0.0.0:$pairing_port" }

[remote_agent.limits]
max_attached_sessions = 8
max_message_bytes = 16384
max_result_bytes = 65536
max_wait_seconds = 60
max_discovery_seconds = 10
audit_retention_days = 90
EOF_CONFIG
}
write_bootstrap() { mkdir -p "$(dirname -- "$2")"; printf 'controller = "unix://%s"\n' "$1" >"$2"; chmod 600 "$2"; }
wait_broker() {
  local home="$1" status="$2"
  local doctor="${status%.json}-doctor.json"
  for _ in {1..600}; do
    if timeout 2 env -u PYTHONPATH TMPDIR="$tmp_dir" "$package_agent" status --xedoc-home "$home" >"$status" 2>/dev/null && "$python_bin" - "$status" <<'PY'
import json,sys
raise SystemExit(0 if json.load(open(sys.argv[1]))['status']=='running' else 1)
PY
    then
      if timeout 2 env -u PYTHONPATH TMPDIR="$tmp_dir" "$package_agent" doctor \
        --timeout 1 --xedoc-home "$home" >"$doctor" 2>/dev/null &&
        "$python_bin" - "$doctor" <<'PY'
import json,sys
report=json.load(open(sys.argv[1], encoding="utf-8"))
checks=report.get("checks", {})
raise SystemExit(
    0
    if report.get("ok") is True
    and checks.get("ipc", {}).get("ok") is True
    and checks.get("ipc", {}).get("status") == "available"
    else 1
)
PY
      then return; fi
    fi
    sleep .05
  done
  fail "broker did not start for $home"
}
assert_jsonl() { "$python_bin" - "$@" <<'PY'
import json,sys
for filename in sys.argv[1:]:
    events=[json.loads(line) for line in open(filename,encoding='utf-8') if line.strip()]
    assert any(event.get('event')=='controllerPassed' for event in events), (filename,events)
PY
}

managed_phase() {
  local dir="$tmp_dir/managed"
  local source_home="$dir/source-home"
  local target_home="$dir/managed-home"
  local source_socket="$dir/source.sock"
  local target_socket="$dir/target.sock"
  local source_workspace="$dir/source-workspace"
  local target_workspace="$dir/managed-workspace"
  mkdir -p "$dir"
  local source_port target_port source_pair_port target_pair_port source_mock_port target_mock_port
  source_port="$(free_port)"; target_port="$(free_port)"; source_pair_port="$(free_port)"; target_pair_port="$(free_port)"
  start_phase managed
  local source_mock_file="$dir/source-mock-port" target_mock_file="$dir/target-mock-port" state="$dir/state.json" evidence="$artifacts/managed-controller.jsonl"
  new_pane source-mock "$(launcher managed-source-mock env TMPDIR="$tmp_dir" "$python_bin" "$repo_root/scripts/remote_agent_pairing_e2e.py" mock --role source --port-file "$source_mock_file" --request-log "$artifacts/managed-source-mock.jsonl" --state-file "$state")" >/dev/null
  new_pane managed-mock "$(launcher managed-target-mock env TMPDIR="$tmp_dir" "$python_bin" "$repo_root/scripts/remote_agent_pairing_e2e.py" mock --role target --port-file "$target_mock_file" --request-log "$artifacts/managed-target-mock.jsonl" --state-file "$state")" >/dev/null
  wait_file "$source_mock_file"; wait_file "$target_mock_file"; source_mock_port="$(<"$source_mock_file")"; target_mock_port="$(<"$target_mock_file")"
  write_config "$source_home" coordinator "$source_workspace" "$source_port" "$source_pair_port" "$source_mock_port"
  write_config "$target_home" managed "$target_workspace" "$target_port" "$target_pair_port" "$target_mock_port"
  write_bootstrap "$source_socket" "$source_home/remote-agent/bootstrap.toml"; write_bootstrap "$target_socket" "$target_home/remote-agent/bootstrap.toml"
  new_pane source-app "$(launcher managed-source-app env XEDOC_HOME="$source_home" TMPDIR="$tmp_dir" "$package_xedoc" app-server --listen unix://"$source_socket")" >/dev/null
  new_pane managed-app "$(launcher managed-target-app env XEDOC_HOME="$target_home" TMPDIR="$tmp_dir" "$package_xedoc" app-server --listen unix://"$target_socket")" >/dev/null
  wait_socket "$source_socket"; wait_socket "$target_socket"
  new_pane source-broker "$(launcher managed-source-broker env XEDOC_HOME="$source_home" TMPDIR="$tmp_dir" "$package_agent" serve --xedoc-home "$source_home")" >/dev/null
  new_pane managed-broker "$(launcher managed-target-broker env XEDOC_HOME="$target_home" TMPDIR="$tmp_dir" "$package_agent" serve --xedoc-home "$target_home")" >/dev/null
  wait_broker "$source_home" "$artifacts/managed-source-status.json"; wait_broker "$target_home" "$artifacts/managed-target-status.json"
  local enrollment_json code
  enrollment_json="$(env -u PYTHONPATH TMPDIR="$tmp_dir" "$package_agent" enrollment create --xedoc-home "$target_home")"
  code="$($python_bin -c 'import json,sys; print(json.load(sys.stdin)["code"])' <<<"$enrollment_json")"
  local selection_downs
  selection_downs="$(menu_index_for_port "$source_home" "$target_port" "$dir/source-discovery.json")"
  local enroll
  enroll="$(new_pane managed-enroll "$(launcher managed-enroll env HOME="$source_home" XEDOC_HOME="$source_home" TMPDIR="$tmp_dir" "$package_session" --socket "$source_socket" remote-agent-enroll)")"
  wait_pane_text "$enroll" 'Remote host (host ID'
  for ((index = 0; index < selection_downs; index++)); do
    tmux send-keys -t "$enroll" Down
  done
  tmux send-keys -t "$enroll" Enter
  wait_pane_text "$enroll" 'Managed-host enrollment code:'
  sleep .2
  tmux send-keys -t "$enroll" -l "$code"; tmux send-keys -t "$enroll" Enter
  wait_pane_exit "$enroll"
  tmux capture-pane -p -t "$enroll" -S -80 >"$artifacts/managed-enroll.log"
  grep -Fq '"pairing"' "$artifacts/managed-enroll.log" || fail 'managed enrollment did not complete pairing'
  "$python_bin" - "$artifacts/managed-enroll.log" "$state" <<'PY'
import json
from pathlib import Path
import sys

entries = []
content = Path(sys.argv[1]).read_text(encoding="utf-8")
for start, character in enumerate(content):
    value = content[start:]
    if character == "{":
        try:
            entries.append(json.JSONDecoder().raw_decode(value)[0])
        except json.JSONDecodeError:
            pass
pairing = next(
    (
        entry["pairing"]
        for entry in entries
        if isinstance(entry, dict)
        and isinstance(entry.get("pairing"), dict)
        and isinstance(entry["pairing"].get("hostId"), str)
    ),
    None,
)
if pairing is None:
    raise SystemExit("managed enrollment result omitted the peer host ID")
state_path = Path(sys.argv[2])
state = json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
state["managedHostId"] = pairing["hostId"]
state_path.write_text(json.dumps(state), encoding="utf-8")
PY
  local controller
  controller="$(new_pane managed-controller "$(launcher managed-controller env TMPDIR="$tmp_dir" "$python_bin" "$repo_root/scripts/remote_agent_pairing_e2e.py" controller --source-socket "$source_socket" --target-socket "$target_socket" --cwd "$source_workspace" --ready-file "$dir/ready.json" --evidence-file "$evidence" --state-file "$state" --timeout "$controller_timeout" --wait-timeout 90)")"
  wait_pane_exit "$controller"; [[ "$(tmux display-message -p -t "$controller" '#{pane_dead_status}')" == 0 ]] || fail 'managed controller failed'
  assert_jsonl "$evidence"
  "$python_bin" - "$evidence" "$state" <<'PY'
import json,sys
items=[json.loads(x) for x in open(sys.argv[1])]
completed=[x for x in items if x.get('event')=='completedTaskResult']
assert len(completed)==1,items
completed=completed[0]
assert completed.get('state')=='completed',completed
assert completed.get('resultStatus')=='completed',completed
assert isinstance(completed.get('outputText'),str),completed
assert 'remote-agent-e2e-tmp-entry' in completed['outputText'],completed
events=completed.get('events')
assert (
    isinstance(events,list)
    and events
    and events[-1]=={'type':'terminal','status':'completed'}
    and all(
        isinstance(event,dict)
        and event.get('type')=='progress'
        and event.get('status')=='running'
        for event in events[:-1]
    )
),completed
assert completed.get('stopReason')=='terminal',completed
assert all(isinstance(completed.get(key),str) and completed[key] for key in ('operation','threadId','turnId')),completed
assert any(
    x.get('event')=='remoteWaitProgress'
    and any(
        event.get('type')=='progress'
        and event.get('status')=='running'
        and isinstance(event.get('activitySummary'),str)
        and event['activitySummary']
        for event in x.get('events',[])
        if isinstance(event,dict)
    )
    for x in items
),items
lifecycles=[x for x in items if x.get('event')=='remoteActivityLifecycle']
assert len(lifecycles)==1,items
lifecycle=lifecycles[0]
remote_session_id=lifecycle.get('remoteSessionId')
statuses=lifecycle.get('statuses')
assert isinstance(remote_session_id,str) and remote_session_id,lifecycle
assert (
    isinstance(statuses,list)
    and 'running' in statuses
    and statuses[-1]=='cancelled'
),lifecycle
assert any(x.get('event')=='targetInterrupted' for x in items),items
state=json.load(open(sys.argv[2]))
assert state.get('targetInterrupted') is True,state
assert state.get('cancellationTerminal') == {
    'state':'cancelled',
    'events':[{'type':'terminal','status':'cancelled'}],
    'stopReason':'terminal',
},state
PY
  stop_phase
}

coordinator_phase() {
  local dir="$tmp_dir/coordinator"
  local source_home="$dir/source-home"
  local target_home="$dir/target-home"
  local source_socket="$dir/source.sock"
  local target_socket="$dir/target.sock"
  local source_workspace="$dir/source-workspace"
  local target_workspace="$dir/target-workspace"
  mkdir -p "$dir"
  local source_port target_port source_pair_port target_pair_port
  source_port="$(free_port)"; target_port="$(free_port)"; source_pair_port="$(free_port)"; target_pair_port="$(free_port)"
  start_phase coordinator
  local source_mock_file="$dir/source-mock-port" target_mock_file="$dir/target-mock-port" state="$dir/state.json" evidence="$artifacts/coordinator-controller.jsonl"
  new_pane coordinator-source-mock "$(launcher coordinator-source-mock env TMPDIR="$tmp_dir" "$python_bin" "$repo_root/scripts/remote_agent_coordinator_pairing_e2e.py" mock --role source --port-file "$source_mock_file" --request-log "$artifacts/coordinator-source-mock.jsonl" --state-file "$state")" >/dev/null
  new_pane coordinator-target-mock "$(launcher coordinator-target-mock env TMPDIR="$tmp_dir" "$python_bin" "$repo_root/scripts/remote_agent_coordinator_pairing_e2e.py" mock --role target --port-file "$target_mock_file" --request-log "$artifacts/coordinator-target-mock.jsonl" --state-file "$state")" >/dev/null
  wait_file "$source_mock_file"; wait_file "$target_mock_file"
  write_config "$source_home" coordinator "$source_workspace" "$source_port" "$source_pair_port" "$(<"$source_mock_file")"
  write_config "$target_home" coordinator "$target_workspace" "$target_port" "$target_pair_port" "$(<"$target_mock_file")"
  write_bootstrap "$source_socket" "$source_home/remote-agent/bootstrap.toml"; write_bootstrap "$target_socket" "$target_home/remote-agent/bootstrap.toml"
  new_pane coordinator-source-app "$(launcher coordinator-source-app env XEDOC_HOME="$source_home" TMPDIR="$tmp_dir" "$package_xedoc" app-server --listen unix://"$source_socket")" >/dev/null
  new_pane coordinator-target-app "$(launcher coordinator-target-app env XEDOC_HOME="$target_home" TMPDIR="$tmp_dir" "$package_xedoc" app-server --listen unix://"$target_socket")" >/dev/null
  wait_socket "$source_socket"; wait_socket "$target_socket"
  new_pane coordinator-source-broker "$(launcher coordinator-source-broker env XEDOC_HOME="$source_home" TMPDIR="$tmp_dir" "$package_agent" serve --xedoc-home "$source_home" --host-id host_source)" >/dev/null
  new_pane coordinator-target-broker "$(launcher coordinator-target-broker env XEDOC_HOME="$target_home" TMPDIR="$tmp_dir" "$package_agent" serve --xedoc-home "$target_home" --host-id host_target)" >/dev/null
  wait_broker "$source_home" "$artifacts/coordinator-source-status.json"; wait_broker "$target_home" "$artifacts/coordinator-target-status.json"
  local controller
  controller="$(new_pane coordinator-controller "$(launcher coordinator-controller env TMPDIR="$tmp_dir" "$python_bin" "$repo_root/scripts/remote_agent_coordinator_pairing_e2e.py" controller --source-socket "$source_socket" --target-socket "$target_socket" --source-cwd "$source_workspace" --target-session-command "$package_session" --target-home "$target_home" --target-env TMPDIR="$tmp_dir" --ready-file "$dir/ready.json" --evidence-file "$evidence" --state-file "$state" --timeout "$controller_timeout" --wait-timeout 90)")"
  wait_pane_exit "$controller"; [[ "$(tmux display-message -p -t "$controller" '#{pane_dead_status}')" == 0 ]] || fail 'coordinator controller failed'
  assert_jsonl "$evidence"
  "$python_bin" - "$evidence" <<'PY'
import json,sys
items=[json.loads(x) for x in open(sys.argv[1])]
assert any(x.get('event')=='pendingPair' for x in items),items
assert any(x.get('event')=='targetCliApproval' and x.get('result',{}).get('status')=='paired' for x in items),items
assert {x.get('event') for x in items} >= {'sourcePairedList','targetPairedList','controllerPassed'},items
PY
  stop_phase
}

managed_phase
coordinator_phase
printf '%s\n' 'PASS: packaged remote-agent pairing tmux E2E'
