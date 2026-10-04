#!/usr/bin/env bash
# Isolated Colima acceptance harness for a packaged macOS coordinator and Linux managed peer.
#
# This intentionally never builds packages and never mounts user credentials.  The package paths
# must be fresh artifacts from the release build.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dockerfile="$script_dir/remote_agent_docker/Dockerfile"
helper="$script_dir/remote_agent_docker_e2e.py"
host_package=""
linux_package=""
target="linux-arm64"
keep="${XEDOC_REMOTE_AGENT_DOCKER_E2E_KEEP:-0}"

usage() {
  cat <<'EOF'
Usage: scripts/test_remote_agent_docker.sh --host-package PATH --linux-package PATH [options]

`--host-package` is an unpacked macOS package directory or package ZIP.
`--linux-package` is a Linux package ZIP matching `--target`.
The harness starts every long-lived process in a named tmux session, uses a
private coordinator XEDOC_HOME, and has no dependency on ~/.xedoc credentials.

Options:
  --host-package PATH       Fresh macOS package directory or ZIP.
  --linux-package PATH      Fresh Linux package ZIP.
  --target TARGET           linux-arm64 (default) or linux-x86_64.
  --keep                    Retain temp artifacts, the test container, and tmux session.
  -h, --help                Show help.
EOF
}
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
outbound_ipv4() {
  python3 - <<'PY'
import ipaddress
import socket

with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
    probe.connect(("8.8.8.8", 80))
    address = probe.getsockname()[0]
parsed = ipaddress.ip_address(address)
if parsed.is_loopback or parsed.is_unspecified:
    raise SystemExit("no non-loopback outbound IPv4 address is available")
print(address)
PY
}
while (($#)); do
  case "$1" in
    --host-package|--linux-package|--target)
      (($# >= 2)) || fail "$1 requires a value"
      case "$1" in --host-package) host_package="$2";; --linux-package) linux_package="$2";; *) target="$2";; esac
      shift 2;;
    --keep) keep=1; shift;;
    -h|--help) usage; exit 0;;
    *) fail "unknown option: $1";;
  esac
done
[[ -n "$host_package" && -n "$linux_package" ]] || fail "--host-package and --linux-package are required"
[[ -f "$helper" ]] || fail "missing Docker E2E helper: $helper"
[[ -e "$host_package" && -f "$linux_package" ]] || fail "package path does not exist"
case "$target" in linux-arm64) platform=linux/arm64;; linux-x86_64) platform=linux/amd64;; *) fail "unsupported target: $target";; esac
command -v docker >/dev/null || fail "docker is required"
command -v tmux >/dev/null || fail "tmux is required"
if [[ -z "${DOCKER_HOST:-}" && -S "$HOME/.colima/default/docker.sock" ]]; then export DOCKER_HOST="unix://$HOME/.colima/default/docker.sock"; fi
docker info >/dev/null || fail "Docker is unavailable; start Colima with: colima start"
advertise_host="$(outbound_ipv4)" ||
  fail "could not determine a non-loopback outbound IPv4 address for Docker peer advertisement"

tmp_base="${TMPDIR:-/tmp}"
if [[ "$(uname -s)" == Darwin ]]; then
  tmp_base="$HOME/.xedoc"
  mkdir -p "$tmp_base"
fi
tmp="$(mktemp -d "$tmp_base/xedoc-remote-agent-docker-e2e.XXXXXX")"
host_tmp="$(mktemp -d "/private/tmp/xra-e2e.XXXXXX")"
export TMPDIR="$tmp_base"
session="xedoc-remote-agent-docker-e2e-$RANDOM-$$"
container="xedoc-remote-agent-e2e-$RANDOM-$$"
capture_diagnostics() {
  while IFS= read -r window; do
    tmux list-panes -t "$window" -F '#{pane_id} #{pane_dead} #{pane_exit_status}' \
      >&2 2>/dev/null || true
    while IFS= read -r pane; do
      printf '\n--- tmux pane %s ---\n' "$pane" >&2
      tmux capture-pane -pt "$pane" -S -160 >&2 2>/dev/null || true
    done < <(tmux list-panes -t "$window" -F '#{pane_id}' 2>/dev/null || true)
  done < <(tmux list-windows -t "$session" -F '#{window_id}' 2>/dev/null || true)
  if docker container inspect "$container" >/dev/null 2>&1; then
    printf '\n--- docker logs %s ---\n' "$container" >&2
    docker logs "$container" >&2 || true
  fi
}
cleanup() {
  local status="${1:-1}"
  if [[ -n "${host_agent:-}" && -n "${home:-}" ]]; then
    "$host_agent" shutdown --xedoc-home "$home" --timeout 5 >/dev/null 2>&1 || true
  fi
  if docker container inspect "$container" >/dev/null 2>&1; then
    docker exec "$container" xedoc-remote-agentd shutdown --timeout 5 >/dev/null 2>&1 || true
  fi
  if [[ "$keep" == 1 || "$status" != 0 ]]; then
    capture_diagnostics
  fi
  if [[ "$keep" != 1 ]]; then
    docker rm -f "$container" >/dev/null 2>&1 || true
    tmux kill-session -t "$session" >/dev/null 2>&1 || true
    chmod -R u+w "$host_tmp" 2>/dev/null || true
    rm -rf "$host_tmp"
  fi
  if [[ "$keep" == 1 || "$status" != 0 ]]; then
    printf 'Docker E2E artifacts retained at %s\n' "$tmp" >&2
  else
    chmod -R u+w "$tmp" 2>/dev/null || true
    rm -rf "$tmp"
  fi
}
trap 'status=$?; cleanup "$status"; exit "$status"' EXIT

host_root="$tmp/host-package"
if [[ -d "$host_package" ]]; then
  cp -R "$host_package" "$host_root"
else
  mkdir -p "$host_root"; unzip -q "$host_package" -d "$host_root"
fi
host_xedoc="$host_root/bin/xedoc"
host_session="$host_root/bin/xedoc-session"
host_agent="$host_root/bin/xedoc-remote-agentd"
[[ -x "$host_xedoc" && -x "$host_session" && -x "$host_agent" ]] || fail "host package is incomplete"

home="$host_tmp/coordinator-home"
socket="$host_tmp/coordinator.sock"
control_dir="$home/app-server-control"
source_workspace="$host_tmp/source-workspace"
assets="$tmp/e2e"
state_file="$assets/state.json"
mkdir -p "$home/remote-agent" "$control_dir" "$source_workspace" "$tmp/build" "$assets"
chmod 700 "$home" "$home/remote-agent"
host_normal=46100; host_pairing=46101; peer_normal=46200; peer_pairing=46201
peer_discovery="$(python3 - <<'PY'
import socket

for port in range(43371, 43375):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        try:
            listener.bind(("127.0.0.1", port))
        except OSError:
            continue
    print(port)
    break
else:
    raise SystemExit("no remote-agent discovery TCP port is available")
PY
)"
cp "$helper" "$script_dir/session_script_sdk.py" "$script_dir/remote_agent_pairing_e2e.py" "$assets/"
source_port_file="$assets/source-port"
target_port_file="$assets/target-port"
source_log="$assets/source-mock.jsonl"
target_log="$assets/target-mock.jsonl"

tmux new-session -d -s "$session" -n e2e "exec env -u PYTHONPATH python3 '$assets/remote_agent_docker_e2e.py' mock --role source --port-file '$source_port_file' --request-log '$source_log' --state-file '$state_file'"
tmux set-option -t "$session":0 remain-on-exit on
tmux set-option -t "$session" remain-on-exit on
tmux split-window -d -t "$session":0 -h "exec env -u PYTHONPATH python3 '$assets/remote_agent_docker_e2e.py' mock --role target --port-file '$target_port_file' --request-log '$target_log' --state-file '$state_file'"
for _ in {1..200}; do [[ -s "$source_port_file" && -s "$target_port_file" ]] && break; sleep 0.05; done
[[ -s "$source_port_file" && -s "$target_port_file" ]] || fail "deterministic Responses mocks did not start"
source_port="$(cat "$source_port_file")"
target_port="$(cat "$target_port_file")"
cat >"$home/config.toml" <<EOF
model = "mock-model"
model_provider = "remote_agent_e2e"
approval_policy = "never"
sandbox_mode = "read-only"
auto_session_name = false
[model_providers.remote_agent_e2e]
name = "Docker remote-agent source mock"
base_url = "http://127.0.0.1:$source_port/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
requires_openai_auth = false
supports_websockets = false
namespace_tools = true

[remote_agent]
role = "coordinator"
workspaces = { workspace_root = "$source_workspace" }
peer_listener = { endpoint = "tls://0.0.0.0:$host_normal", pairing_endpoint = "tls://0.0.0.0:$host_pairing", advertised_endpoint = "tls://host.docker.internal:$host_normal", advertised_pairing_endpoint = "tls://host.docker.internal:$host_pairing" }
[remote_agent.limits]
max_attached_sessions = 8
max_message_bytes = 16384
max_result_bytes = 65536
max_wait_seconds = 120
max_discovery_seconds = 10
audit_retention_days = 90
EOF
printf 'controller = "unix://%s"\n' "$socket" >"$home/remote-agent/bootstrap.toml"
chmod 600 "$home/remote-agent/bootstrap.toml"
target_config="$tmp/target-config.toml"
target_bootstrap="$tmp/target-bootstrap.toml"
cat >"$target_config" <<EOF
model = "mock-model"
model_provider = "remote_agent_e2e"
approval_policy = "never"
sandbox_mode = "read-only"
auto_session_name = false
[model_providers.remote_agent_e2e]
name = "Docker remote-agent target mock"
base_url = "http://host.docker.internal:$target_port/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
requires_openai_auth = false
supports_websockets = false
namespace_tools = true

[remote_agent]
role = "managed"
workspaces = { workspace_root = "/" }
peer_listener = { endpoint = "tls://0.0.0.0:46000", pairing_endpoint = "tls://0.0.0.0:46001", advertised_endpoint = "tls://$advertise_host:$peer_normal", advertised_pairing_endpoint = "tls://$advertise_host:$peer_pairing" }
[remote_agent.limits]
max_attached_sessions = 8
max_message_bytes = 16384
max_result_bytes = 65536
max_wait_seconds = 120
max_discovery_seconds = 10
audit_retention_days = 90
EOF
printf 'controller = "unix:///root/.xedoc/app-server-control/app-server-control.sock"\n' >"$target_bootstrap"
cp "$linux_package" "$tmp/build/package.zip"
docker build --platform "$platform" --tag "$container:latest" --file "$dockerfile" "$tmp/build"

tmux split-window -d -t "$session":0 -v "exec env -u PYTHONPATH XEDOC_HOME='$home' '$host_xedoc' app-server --listen unix://$socket"
tmux split-window -d -t "$session":0 -v "exec docker run --rm --name '$container' --platform '$platform' --add-host host.docker.internal:host-gateway -p 0.0.0.0:$peer_normal:46000/tcp -p 0.0.0.0:$peer_pairing:46001/tcp -p 127.0.0.1:$peer_discovery:$peer_discovery/tcp -v '$assets:/e2e' '$container:latest'"
for _ in {1..200}; do
  [[ "$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null || true)" == true ]] && break
  sleep 0.05
done
[[ "$(docker inspect --format '{{.State.Running}}' "$container" 2>/dev/null || true)" == true ]] ||
  fail "Docker managed-peer container did not start"
for _ in {1..200}; do
  docker exec "$container" test -f /e2e/remote_agent_docker_e2e.py && break
  sleep 0.05
done
docker exec "$container" test -f /e2e/remote_agent_docker_e2e.py ||
  fail "Docker managed-peer container did not mount E2E helpers"
docker cp "$target_config" "$container:/root/.xedoc/config.toml"
docker exec "$container" mkdir -p /root/.xedoc/remote-agent
docker cp "$target_bootstrap" "$container:/root/.xedoc/remote-agent/bootstrap.toml"
docker exec "$container" chown root:root /root/.xedoc/remote-agent/bootstrap.toml
for _ in {1..200}; do [[ -S "$socket" ]] && break; sleep 0.05; done
[[ -S "$socket" ]] || fail "coordinator app server did not create its socket"
tmux split-window -d -t "$session":0 -v "exec sh -ceu 'for i in \$(seq 1 120); do env -u PYTHONPATH XEDOC_HOME=\"$home\" \"$host_agent\" ensure --xedoc-home \"$home\" && exit 0; sleep 1; done; exit 1'"
tmux split-window -d -t "$session":0 -v "exec docker exec '$container' sh -ceu 'chmod 700 /root/.xedoc/remote-agent; chmod 600 /root/.xedoc/remote-agent/bootstrap.toml; xedoc app-server daemon start || true; for i in \$(seq 1 120); do xedoc-remote-agentd ensure && exit 0; sleep 1; done; exit 1'"
for _ in {1..200}; do
  host_status="$("$host_agent" status --xedoc-home "$home" 2>/dev/null || true)"
  managed_status="$(docker exec "$container" xedoc-remote-agentd status 2>/dev/null || true)"
  [[ "$host_status" == *'"status":"running"'* && "$managed_status" == *'"status":"running"'* ]] && break
  sleep 0.1
done
[[ "$host_status" == *'"status":"running"'* ]] || fail "coordinator broker did not start"
[[ "$managed_status" == *'"status":"running"'* ]] || fail "managed broker did not start"
tmux new-window -d -t "$session" -n discovery-relay-server "exec docker exec '$container' env PYTHONPATH=/e2e python3 /e2e/remote_agent_docker_e2e.py discovery-relay-server --tcp-port $peer_discovery --udp-port 43371"
tmux new-window -d -t "$session" -n discovery-relay-client "exec env PYTHONPATH='$assets' python3 '$assets/remote_agent_docker_e2e.py' discovery-relay-client --udp-port $peer_discovery --tcp-port $peer_discovery"
sleep 0.2
enrollment="$(docker exec "$container" xedoc-remote-agentd enrollment create | python3 -c 'import json,sys; print(json.load(sys.stdin)["code"])')"

# This is the actual public managed enrollment flow. getpass reads the code from the tmux pane;
# no certificate is copied into either home and no broker state is mutated by the harness.
tmux new-window -d -t "$session" -n enroll "exec env -u PYTHONPATH XEDOC_HOME='$home' '$host_session' remote-agent-enroll --endpoint 127.0.0.1:$peer_discovery"
tmux set-window-option -t "$session":enroll remain-on-exit on
enroll_pane="$(tmux list-panes -t "$session":enroll -F '#{pane_id}')"
for _ in {1..200}; do
  tmux capture-pane -pt "$enroll_pane" -S -60 | grep -q 'Managed host to enroll' && break
  sleep 0.05
done
tmux capture-pane -pt "$enroll_pane" -S -60 | grep -q 'Managed host to enroll' ||
  fail "coordinator did not discover the managed peer"
tmux send-keys -t "$enroll_pane" Enter
for _ in {1..200}; do
  tmux capture-pane -pt "$enroll_pane" -S -60 | grep -q 'enrollment code' && break
  sleep 0.05
done
tmux send-keys -t "$enroll_pane" -l "$enrollment"
tmux send-keys -t "$enroll_pane" Enter
for _ in {1..600}; do
  tmux capture-pane -pt "$enroll_pane" -S -100 | grep -q '"pairing"' && break
  sleep 0.1
done
tmux capture-pane -pt "$enroll_pane" -S -160 >"$tmp/enrollment.log"
grep -q '"pairing"' "$tmp/enrollment.log" || fail "managed enrollment did not produce a pairing result"
env -u PYTHONPATH XEDOC_HOME="$home" "$host_agent" enrollment discover --xedoc-home "$home" >"$tmp/discovered-hosts.json"
python3 - "$tmp/discovered-hosts.json" "$state_file" <<'PY'
import json
from pathlib import Path
import sys

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
entries = value.get("data") if isinstance(value, dict) else None
host_ids = [
    entry.get("hostId")
    for entry in entries
    if isinstance(entry, dict)
    and entry.get("role") == "managed"
    and entry.get("status") == "paired"
    and isinstance(entry.get("hostId"), str)
]
if len(host_ids) != 1:
    raise SystemExit("managed enrollment did not produce exactly one paired host")
state_path = Path(sys.argv[2])
state = json.loads(state_path.read_text(encoding="utf-8")) if state_path.exists() else {}
state["managedHostId"] = host_ids[0]
state_path.write_text(json.dumps(state), encoding="utf-8")
PY
docker exec "$container" xedoc-remote-agentd status >"$tmp/managed-status.json"
"$host_agent" status --xedoc-home "$home" >"$tmp/coordinator-status.json"
tmux new-window -d -t "$session" -n observer "exec docker exec '$container' env PYTHONPATH=/e2e python3 /e2e/remote_agent_docker_e2e.py target-observer --socket /root/.xedoc/app-server-control/app-server-control.sock --state-file /e2e/state.json --timeout 30"
tmux set-window-option -t "$session":observer remain-on-exit on
observer_pane="$(tmux list-panes -t "$session":observer -F '#{pane_id}')"
tmux new-window -d -t "$session" -n controller "exec env PYTHONPATH='$assets' python3 '$assets/remote_agent_docker_e2e.py' source-controller --socket '$socket' --cwd '$source_workspace' --state-file '$state_file' --timeout 30"
tmux set-window-option -t "$session":controller remain-on-exit on
controller_pane="$(tmux list-panes -t "$session":controller -F '#{pane_id}')"
for _ in {1..2400}; do
  state="$(cat "$state_file" 2>/dev/null || true)"
  printf '%s' "$state" | grep -q '"controllerPassed":true' && break
  sleep 0.05
done
python3 - "$state_file" <<'PY'
import json
from pathlib import Path
import sys
state = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
for key in (
    "grantObserved",
    "targetInterrupted",
    "controllerPassed",
):
    if state.get(key) is not True:
        raise SystemExit(f"missing acceptance evidence: {key}")
cancellation = state.get("cancellationTerminal")
if (
    not isinstance(cancellation, dict)
    or cancellation.get("state") != "cancelled"
    or cancellation.get("events") != [{"type": "terminal", "status": "cancelled"}]
    or cancellation.get("stopReason") != "terminal"
):
    raise SystemExit("cancellationTerminal is not terminal cancelled evidence")
result = state.get("completedTaskResult")
if not isinstance(result, dict):
    raise SystemExit("completedTaskResult is not an object")
if (
    result.get("state") != "completed"
    or result.get("resultStatus") != "completed"
    or result.get("events") != [{"type": "terminal", "status": "completed"}]
    or result.get("stopReason") != "terminal"
):
    raise SystemExit("completedTaskResult is not terminal completed evidence")
if not all(isinstance(result.get(key), str) and result[key] for key in ("operation", "threadId", "turnId")):
    raise SystemExit("completedTaskResult omits terminal identifiers")
PY
printf 'PASS: pairing, grant, remote session progress, cancellation, and target interruption succeeded; artifacts: %s\n' "$tmp"
