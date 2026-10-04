#!/usr/bin/env bash

# Packaged two-host remote-agent acceptance harness.
#
# The product processes run from an installed package with separate homes and
# an empty PYTHONPATH. Test-only controller and mock processes run outside the
# checkout too, but use the checked-in SDK to drive the public app-server API.

set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly script_dir
repo_root="$(cd -- "$script_dir/.." && pwd -P)"
readonly repo_root
readonly support="$script_dir/remote_agent_tmux_e2e.py"
python_bin="${XEDOC_REMOTE_AGENT_E2E_PYTHON:-python3}"
readonly keep_artifacts="${XEDOC_REMOTE_AGENT_E2E_KEEP_ARTIFACTS:-0}"
readonly install_zip="${XEDOC_REMOTE_AGENT_E2E_INSTALL_ZIP:-}"
install_archive="$install_zip"

case "$(uname -s)" in
  Darwin | Linux) ;;
  *)
    printf '%s\n' 'SKIP: remote-agent Windows support is intentionally deferred.'
    exit 0
    ;;
esac

if [[ -n "${XEDOC_REMOTE_AGENT_E2E_TMPDIR:-}" ]]; then
  tmp_base="$XEDOC_REMOTE_AGENT_E2E_TMPDIR"
elif [[ "$(uname -s)" == Darwin ]]; then
  tmp_base="/private/tmp"
else
  tmp_base="${TMPDIR:-/tmp}"
fi
mkdir -p -m 700 "$tmp_base"
tmp_base="$(cd -- "$tmp_base" && pwd -P)"
tmp_dir="$(mktemp -d "$tmp_base/xra.XXXXXX")"
readonly tmp_dir
readonly outside_dir="$tmp_dir/outside"
readonly artifacts="$tmp_dir/artifacts"
readonly coordinator_home="$tmp_dir/coordinator-home"
readonly managed_home="$tmp_dir/managed-home"
readonly coordinator_workspace="$tmp_dir/coordinator-workspace"
readonly managed_workspace="$tmp_dir/managed-workspace"
readonly coordinator_socket="$tmp_dir/coordinator-app-server.sock"
readonly managed_socket="$tmp_dir/managed-app-server.sock"
readonly coordinator_certificate="$artifacts/coordinator-cert.pem"
readonly managed_certificate="$artifacts/managed-cert.pem"
readonly coordinator_bootstrap="$coordinator_home/remote-agent/bootstrap.toml"
readonly managed_bootstrap="$managed_home/remote-agent/bootstrap.toml"
readonly mock_port_file="$artifacts/mock-port"
readonly mock_log="$artifacts/mock.jsonl"
readonly source_ready="$artifacts/source-ready.json"
readonly target_ready="$artifacts/target-ready.json"
readonly source_log="$artifacts/source-controller.jsonl"
readonly target_log="$artifacts/target-controller.jsonl"
readonly coordinator_status="$artifacts/coordinator-status.json"
readonly managed_status="$artifacts/managed-status.json"
readonly coordinator_doctor="$artifacts/coordinator-doctor.json"
readonly managed_doctor="$artifacts/managed-doctor.json"
readonly coordinator_audit="$artifacts/coordinator-audit.jsonl"
readonly managed_audit="$artifacts/managed-audit.jsonl"
readonly install_home="$tmp_dir/install-home"
readonly install_xedoc_home="$install_home/.xedoc"
readonly install_bin="$install_home/.local/bin"
readonly install_workspace="$tmp_dir/install-workspace"
readonly install_experimental_socket="$install_xedoc_home/app-server-control/experimental.sock"
readonly install_stable_socket="$install_xedoc_home/app-server-control/app-server-control.sock"
readonly install_broker_pid="$install_xedoc_home/remote-agent/broker.pid"
readonly install_experimental_bootstrap="$install_xedoc_home/remote-agent/bootstrap-experimental.toml"
readonly install_stable_bootstrap="$install_xedoc_home/remote-agent/bootstrap.toml"

tmux_session=""
mock_pid=""
source_controller_pid=""
target_controller_pid=""
package_root=""
package_xedoc=""
package_agent=""
package_session=""

capture_diagnostics() {
  printf '\n--- remote-agent E2E artifacts: %s ---\n' "$artifacts" >&2
  if [[ -n "$tmux_session" ]]; then
    tmux list-panes -t "$tmux_session":0 -F '#{pane_id} #{pane_title} #{pane_dead}' \
      2>/dev/null >&2 || true
    while IFS= read -r pane; do
      printf '\n--- tmux pane %s ---\n' "$pane" >&2
      tmux capture-pane -p -t "$pane" -S -160 2>/dev/null >&2 || true
    done < <(tmux list-panes -t "$tmux_session":0 -F '#{pane_id}' 2>/dev/null || true)
  fi
  for path in \
    "$artifacts"/*.stdout.log \
    "$artifacts"/*.stderr.log \
    "$mock_log" \
    "$source_log" \
    "$target_log" \
    "$coordinator_status" \
    "$managed_status" \
    "$coordinator_doctor" \
    "$managed_doctor" \
    "$coordinator_audit" \
    "$managed_audit" \
    "$artifacts/install-experimental.stdout.log" \
    "$artifacts/install-experimental.stderr.log" \
    "$artifacts/install-stable.stdout.log" \
    "$artifacts/install-stable.stderr.log" \
    "$artifacts/configure-experimental.log" \
    "$artifacts/configure-stable.log" \
    "$artifacts/install-experimental-broker-doctor.json" \
    "$artifacts/install-broker-doctor.json"
  do
    [[ -f "$path" ]] || continue
    printf '\n--- %s ---\n' "$(basename -- "$path")" >&2
    tail -n 160 "$path" >&2 || true
  done
}

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  capture_diagnostics
  exit 1
}

cleanup() {
  local status="$1"
  if [[ -x "$install_bin/xedoc" ]]; then
    env HOME="$install_home" XEDOC_HOME="$install_xedoc_home" \
      "$install_bin/xedoc" app-server daemon stop >/dev/null 2>&1 || true
  fi
  if [[ -x "$install_bin/xedoc-experimental" ]]; then
    env HOME="$install_home" XEDOC_HOME="$install_xedoc_home" \
      "$install_bin/xedoc-experimental" app-server daemon stop >/dev/null 2>&1 || true
  fi
  if [[ -x "$install_xedoc_home/packages/standalone/experimental/bin/xedoc-remote-agentd" ]]; then
    "$install_xedoc_home/packages/standalone/experimental/bin/xedoc-remote-agentd" \
      shutdown --xedoc-home "$install_xedoc_home" >/dev/null 2>&1 || true
  fi
  [[ -n "$tmux_session" ]] &&
    tmux kill-session -t "$tmux_session" >/dev/null 2>&1 || true
  [[ -n "$mock_pid" ]] && kill "$mock_pid" >/dev/null 2>&1 || true
  [[ -n "$mock_pid" ]] && wait "$mock_pid" >/dev/null 2>&1 || true
  [[ -n "$source_controller_pid" ]] &&
    kill "$source_controller_pid" >/dev/null 2>&1 || true
  [[ -n "$source_controller_pid" ]] &&
    wait "$source_controller_pid" >/dev/null 2>&1 || true
  [[ -n "$target_controller_pid" ]] &&
    kill "$target_controller_pid" >/dev/null 2>&1 || true
  [[ -n "$target_controller_pid" ]] &&
    wait "$target_controller_pid" >/dev/null 2>&1 || true
  if [[ "$status" -eq 0 && "$keep_artifacts" != 1 ]]; then
    rm -rf -- "$tmp_dir"
  else
    printf 'Remote-agent E2E artifacts retained at %s\n' "$tmp_dir" >&2
  fi
}

trap 'cleanup "$?"' EXIT INT TERM HUP

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

free_port() {
  "$python_bin" - <<'PY'
import socket

with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    print(probe.getsockname()[1])
PY
}

wait_for_file() {
  local path="$1"
  for _ in $(seq 1 600); do
    [[ -s "$path" ]] && return
    sleep 0.05
  done
  fail "timed out waiting for $path"
}

wait_for_unix_listener() {
  local path="$1"
  "$python_bin" - "$path" <<'PY'
import socket
import sys
import time

path = sys.argv[1]
for _ in range(600):
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.settimeout(0.1)
    try:
        connection.connect(path)
    except OSError:
        time.sleep(0.05)
    else:
        connection.close()
        raise SystemExit(0)
    finally:
        try:
            connection.close()
        except OSError:
            pass
raise SystemExit(f"timed out waiting for Unix listener {path}")
PY
}

wait_for_process() {
  local pid="$1"
  local label="$2"
  for _ in $(seq 1 1800); do
    if ! kill -0 "$pid" >/dev/null 2>&1; then
      wait "$pid" || fail "$label exited unsuccessfully"
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for $label"
}

wait_for_tmux_text() {
  local target="$1"
  local expected="$2"
  for _ in $(seq 1 600); do
    if tmux capture-pane -p -t "$target" -S -80 2>/dev/null |
      grep -F -- "$expected" >/dev/null
    then
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for tmux prompt: $expected"
}

wait_for_tmux_exit() {
  local target="$1"
  for _ in $(seq 1 600); do
    if [ "$(tmux display-message -p -t "$target" '#{pane_dead}' 2>/dev/null)" = 1 ]; then
      return
    fi
    sleep 0.05
  done
  fail "timed out waiting for tmux configuration to exit"
}

broker_pid() {
  "$python_bin" - "$install_broker_pid" <<'PY'
import json
from pathlib import Path
import sys

value = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
pid = value.get("pid")
if not isinstance(pid, int) or isinstance(pid, bool) or pid < 1:
    raise SystemExit("broker pid state is invalid")
print(pid)
PY
}

wait_for_broker() {
  local agent="$1"
  local expected_pid="${2:-}"
  for _ in $(seq 1 600); do
    if "$agent" status --xedoc-home "$install_xedoc_home" \
      >"$artifacts/install-broker-status.json" 2>"$artifacts/install-broker-status.stderr.log" &&
      [ -f "$install_broker_pid" ]
    then
      local pid
      pid="$(broker_pid 2>/dev/null || true)"
      if [ -n "$pid" ] && kill -0 "$pid" >/dev/null 2>&1 &&
        { [ -z "$expected_pid" ] || [ "$pid" = "$expected_pid" ]; }
      then
        return
      fi
    fi
    sleep 0.05
  done
  fail "remote-agent broker did not reach the expected single-process state"
}

assert_single_broker_owner() {
  local owner_files=()
  while IFS= read -r path; do
    owner_files+=("$path")
  done < <(find "$install_xedoc_home" -path '*/remote-agent/broker.pid' -type f -print)
  [[ "${#owner_files[@]}" -eq 1 && "${owner_files[0]}" = "$install_broker_pid" ]] ||
    fail "expected exactly one remote-agent broker owner state file"
}

run_installer() {
  local channel="$1"
  shift
  env \
    HOME="$install_home" \
    XEDOC_HOME="$install_xedoc_home" \
    XEDOC_INSTALL_DIR="$install_bin" \
    XEDOC_NON_INTERACTIVE=1 \
    PATH="$PATH" \
    /bin/sh "$repo_root/scripts/install/install.sh" "$@" \
    >"$artifacts/install-$channel.stdout.log" \
    2>"$artifacts/install-$channel.stderr.log" ||
    fail "$channel local ZIP installation failed"
}

stage_install_archive() {
  local runtime_asset
  runtime_asset="$(
    unzip -p "$install_zip" xedoc-package.json | "$python_bin" -c \
      'import json, sys; print(json.load(sys.stdin)["modelRouterRuntime"]["assetName"])'
  )" || fail "could not read model-router runtime metadata from $install_zip"
  [[ -n "$runtime_asset" ]] || fail "local release ZIP has no model-router runtime asset"
  [[ -f "$(dirname -- "$install_zip")/$runtime_asset" ]] && return

  local runtime_candidates=()
  while IFS= read -r candidate; do
    runtime_candidates+=("$candidate")
  done < <(
    find "$repo_root/dist/xedoc/model-router-runtime" -type f -name "$runtime_asset" \
      -print 2>/dev/null
  )
  [[ "${#runtime_candidates[@]}" -eq 1 ]] || fail \
    "could not locate the model-router runtime companion for $install_zip"

  local staged_release="$tmp_dir/local-release"
  mkdir -p "$staged_release"
  cp "$install_zip" "$staged_release/"
  cp "${runtime_candidates[0]}" "$staged_release/$runtime_asset"
  install_archive="$staged_release/$(basename -- "$install_zip")"
}

run_remote_agent_configuration() {
  local channel="$1"
  local session_binary="$2"
  local reconfigure="$3"
  local workspace_id="$4"
  local log="$artifacts/configure-$channel.log"
  local launcher="$artifacts/configure-$channel.sh"

  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n'
    printf 'unset PYTHONPATH\n'
    printf 'env HOME=%q XEDOC_HOME=%q XEDOC_INSTALL_DIR=%q PATH=%q %q remote-agent-configure' \
      "$install_home" "$install_xedoc_home" "$install_bin" "$PATH" "$session_binary"
    if [ "$reconfigure" = true ]; then
      printf ' --reconfigure'
    fi
    printf ' 2>&1 | tee %q\n' "$log"
  } >"$launcher"
  chmod 700 "$launcher"

  tmux_session="xedoc-remote-agent-configure-$channel-$RANDOM-$$"
  tmux new-session -d -x 160 -y 40 -s "$tmux_session" "$launcher"
  tmux set-option -t "$tmux_session":0 remain-on-exit on

  wait_for_tmux_text "$tmux_session":0.0 "Remote-agent role (use ↑/↓ and Enter):"
  # Exercise arrow navigation without changing the coordinator default.
  tmux send-keys -t "$tmux_session":0.0 Down Up Enter
  wait_for_tmux_text "$tmux_session":0.0 "Workspace ID:"
  tmux send-keys -t "$tmux_session":0.0 -l "$workspace_id"
  tmux send-keys -t "$tmux_session":0.0 Enter
  wait_for_tmux_text "$tmux_session":0.0 "Workspace root:"
  tmux send-keys -t "$tmux_session":0.0 -l "$install_workspace"
  tmux send-keys -t "$tmux_session":0.0 Enter
  wait_for_tmux_text "$tmux_session":0.0 "Add another workspace"
  tmux send-keys -t "$tmux_session":0.0 Enter
  wait_for_tmux_exit "$tmux_session":0.0
  grep -F '"status": "configured"' "$log" >/dev/null ||
    fail "$channel remote-agent configuration did not complete"
  tmux kill-session -t "$tmux_session" >/dev/null 2>&1 || true
  tmux_session=""
}

exercise_installed_lifecycle() {
  [[ -n "$install_zip" ]] || return
  [[ -f "$install_zip" ]] || fail \
    "XEDOC_REMOTE_AGENT_E2E_INSTALL_ZIP does not name a local release ZIP: $install_zip"

  mkdir -p "$install_home" "$install_workspace"
  stage_install_archive
  run_installer experimental --experimental --local-zip "$install_archive"
  [[ -x "$install_bin/xedoc-experimental" ]] ||
    fail "experimental installer did not expose xedoc-experimental"
  [[ -x "$install_bin/xedoc-session-experimental" ]] ||
    fail "experimental installer did not expose xedoc-session-experimental"

  run_remote_agent_configuration \
    experimental \
    "$install_bin/xedoc-session-experimental" \
    false \
    experimental
  wait_for_unix_listener "$install_experimental_socket"
  local experimental_agent="$install_xedoc_home/packages/standalone/experimental/bin/xedoc-remote-agentd"
  [[ -x "$experimental_agent" ]] || fail "experimental package lacks xedoc-remote-agentd"
  wait_for_broker "$experimental_agent"
  local broker_before_restart
  broker_before_restart="$(broker_pid)"
  env HOME="$install_home" XEDOC_HOME="$install_xedoc_home" \
    "$install_bin/xedoc-experimental" app-server daemon start \
    >"$artifacts/experimental-daemon-start.stdout.log" \
    2>"$artifacts/experimental-daemon-start.stderr.log" ||
    fail "experimental app-server daemon start failed"
  wait_for_unix_listener "$install_experimental_socket"
  wait_for_broker "$experimental_agent" "$broker_before_restart"
  assert_single_broker_owner
  [[ -f "$install_experimental_bootstrap" ]] ||
    fail "experimental remote-agent bootstrap was not written"
  env \
    XEDOC_REMOTE_AGENT_BOOTSTRAP="$install_experimental_bootstrap" \
    "$experimental_agent" doctor --xedoc-home "$install_xedoc_home" \
    >"$artifacts/install-experimental-broker-doctor.json" ||
    fail "experimental remote-agent broker doctor failed"
  "$python_bin" - "$artifacts/install-experimental-broker-doctor.json" <<'PY'
import json
from pathlib import Path
import sys

assert json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))["ok"] is True
PY

  run_installer stable --local-zip "$install_archive"
  [[ -x "$install_bin/xedoc" ]] || fail "stable installer did not expose xedoc"
  [[ -x "$install_bin/xedoc-session" ]] ||
    fail "stable installer did not expose xedoc-session"
  run_remote_agent_configuration stable "$install_bin/xedoc-session" true stable
  wait_for_unix_listener "$install_stable_socket"
  local stable_agent="$install_xedoc_home/packages/standalone/current/bin/xedoc-remote-agentd"
  [[ -x "$stable_agent" ]] || fail "stable package lacks xedoc-remote-agentd"
  env HOME="$install_home" XEDOC_HOME="$install_xedoc_home" \
    "$install_bin/xedoc" app-server daemon start \
    >"$artifacts/stable-daemon-start.stdout.log" \
    2>"$artifacts/stable-daemon-start.stderr.log" ||
    fail "stable app-server daemon start failed"
  wait_for_unix_listener "$install_stable_socket"
  wait_for_broker "$stable_agent"
  local broker_after_handoff
  broker_after_handoff="$(broker_pid)"
  [ "$broker_after_handoff" != "$broker_before_restart" ] ||
    fail "stable remote-agent handoff did not replace the experimental broker"
  ! kill -0 "$broker_before_restart" >/dev/null 2>&1 ||
    fail "experimental remote-agent broker remained alive after stable handoff"
  assert_single_broker_owner
  [[ -f "$install_stable_bootstrap" ]] ||
    fail "stable remote-agent bootstrap was not written"
  "$stable_agent" doctor --xedoc-home "$install_xedoc_home" \
    >"$artifacts/install-broker-doctor.json" ||
    fail "stable remote-agent broker doctor failed after handoff"
  "$python_bin" - "$artifacts/install-broker-doctor.json" <<'PY'
import json
from pathlib import Path
import sys

assert json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))["ok"] is True
PY

  package_root="$install_xedoc_home/packages/standalone/experimental"
}

assert_binding_provenance() {
  "$python_bin" - "$1" <<'PY'
import json
import os
from pathlib import Path
import socket
import struct
import sys

def receive_exact(connection, length):
    chunks = []
    while length:
        chunk = connection.recv(length)
        if not chunk:
            raise RuntimeError("broker closed the spoofing check")
        chunks.append(chunk)
        length -= len(chunk)
    return b"".join(chunks)

home = Path(sys.argv[1])
directory = home / "remote-agent"
request = {
    "capability": (directory / "broker.capability").read_text(encoding="utf-8").strip(),
    "requestId": "e2e-spoofed-bind",
    "method": "extension/bind",
    "params": {
        "threadId": "thread_spoofed",
        "extensionId": "extension_spoofed",
        "bindingToken": (directory / "extension.capability").read_text(
            encoding="utf-8"
        ).strip(),
    },
}
encoded = json.dumps(request, separators=(",", ":")).encode("utf-8")
with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
    connection.connect(os.fspath(directory / "broker.sock"))
    connection.sendall(struct.pack(">I", len(encoded)) + encoded)
    length = struct.unpack(">I", receive_exact(connection, 4))[0]
    response = json.loads(receive_exact(connection, length))
error = response.get("error")
raise SystemExit(
    0
    if isinstance(error, dict) and error.get("code") == "unauthorized"
    else "broker capability incorrectly minted a source lease"
)
PY
}

write_launcher() {
  local name="$1"
  shift
  local launcher="$artifacts/$name.sh"
  {
    printf '#!/usr/bin/env bash\n'
    printf 'set -euo pipefail\n'
    printf 'cd -- %q\n' "$outside_dir"
    printf 'unset PYTHONPATH\n'
    printf 'exec '
    printf '%q ' "$@"
    printf '\n'
  } >"$launcher"
  chmod 700 "$launcher"
  printf '%s\n' "$launcher"
}

prepare_package() {
  if [[ -n "$package_root" ]]; then
    :
  elif [[ -n "${XEDOC_REMOTE_AGENT_E2E_PACKAGE:-}" ]]; then
    package_root="$(
      "$python_bin" - "${XEDOC_REMOTE_AGENT_E2E_PACKAGE}" <<'PY'
from pathlib import Path
import sys

print(Path(sys.argv[1]).resolve())
PY
    )"
  else
    [[ "${XEDOC_REMOTE_AGENT_E2E_BUILD:-0}" == 1 ]] || fail \
      "set XEDOC_REMOTE_AGENT_E2E_PACKAGE to a package with bin/xedoc and bin/xedoc-remote-agentd, or set XEDOC_REMOTE_AGENT_E2E_BUILD=1 with prebuilt package inputs"
    local xedoc_bin="${XEDOC_REMOTE_AGENT_E2E_XEDOC_BIN:-}"
    local runtime_dir="${XEDOC_REMOTE_AGENT_E2E_REMOTE_AGENT_RUNTIME_DIR:-}"
    local runtime_id="${XEDOC_REMOTE_AGENT_E2E_MODEL_ROUTER_RUNTIME_ID:-}"
    local runtime_asset="${XEDOC_REMOTE_AGENT_E2E_MODEL_ROUTER_RUNTIME_ASSET:-}"
    local runtime_sha256="${XEDOC_REMOTE_AGENT_E2E_MODEL_ROUTER_RUNTIME_SHA256:-}"
    local runtime_tag="${XEDOC_REMOTE_AGENT_E2E_MODEL_ROUTER_RUNTIME_SOURCE_RELEASE_TAG:-}"
    [[ -x "$xedoc_bin" ]] || fail \
      "XEDOC_REMOTE_AGENT_E2E_XEDOC_BIN must be an executable prebuilt package entrypoint"
    [[ -n "$runtime_dir" ]] || fail \
      "XEDOC_REMOTE_AGENT_E2E_REMOTE_AGENT_RUNTIME_DIR must provide a prebuilt Python runtime"
    [[ -n "$runtime_id" && -n "$runtime_asset" && -n "$runtime_sha256" && -n "$runtime_tag" ]] ||
      fail "set all XEDOC_REMOTE_AGENT_E2E_MODEL_ROUTER_RUNTIME_* metadata variables"
    package_root="$tmp_dir/package"
    local package_args=(
      "$python_bin" "$repo_root/scripts/build_xedoc_package.py"
      --package-dir "$package_root"
      --force
      --include-session-control
      --entrypoint-bin "$xedoc_bin"
      --rg-bin "${XEDOC_REMOTE_AGENT_E2E_RG_BIN:-$(command -v rg)}"
      --remote-agent-runtime-dir "$runtime_dir"
      --model-router-runtime-id "$runtime_id"
      --model-router-runtime-asset "$runtime_asset"
      --model-router-runtime-sha256 "$runtime_sha256"
      --model-router-runtime-source-release-tag "$runtime_tag"
    )
    "${package_args[@]}" >"$artifacts/package-build.stdout.log" \
      2>"$artifacts/package-build.stderr.log" ||
      fail "packaged remote-agent build failed"
  fi
  package_xedoc="$package_root/bin/xedoc"
  package_agent="$package_root/bin/xedoc-remote-agentd"
  package_session="$package_root/bin/xedoc-session"
  [[ -x "$package_xedoc" ]] || fail "missing packaged xedoc binary: $package_xedoc"
  [[ -x "$package_agent" ]] || fail "missing packaged remote-agent daemon: $package_agent"
  [[ -x "$package_session" ]] || fail "missing packaged xedoc-session: $package_session"
  [[ -f "$package_root/xedoc-resources/remote-agent/remote-agent.pyz" ]] ||
    fail "package does not contain the remote-agent payload"
}

write_config() {
  local role="$1"
  local home="$2"
  local workspace="$3"
  local listener="$4"
  local peer_listener="$5"
  local peer_certificate="$6"
  local managed_certificate_path="$7"
  local mock_port="$8"
  local approval_policy="never"
  [[ "$role" == "managed" ]] && approval_policy="on-request"
  cat >"$home/config.toml" <<EOF
model = "mock-model"
model_provider = "remote_agent_e2e"
approval_policy = "$approval_policy"
sandbox_mode = "read-only"
auto_session_name = false

[model_providers.remote_agent_e2e]
name = "Remote agent E2E mock"
base_url = "http://127.0.0.1:$mock_port/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
requires_openai_auth = false
supports_websockets = false
namespace_tools = true

[remote_agent]
role = "$role"
workspaces = { e2e = "$workspace" }
peer_listener = { endpoint = "$listener" }
static_peers = [{ endpoint = "$peer_listener", certificate_path = "$peer_certificate" }]
managed_coordinator_certificate_paths = [$managed_certificate_path]

[remote_agent.limits]
max_attached_sessions = 8
max_message_bytes = 16384
max_result_bytes = 65536
max_wait_seconds = 60
max_discovery_seconds = 10
audit_retention_days = 90
EOF
}

write_bootstrap() {
  local path="$1"
  local socket_path="$2"
  mkdir -p "$(dirname -- "$path")"
  printf 'controller = "unix://%s"\n' "$socket_path" >"$path"
  chmod 600 "$path"
}

assert_evidence() {
  "$python_bin" - \
    "$source_log" \
    "$target_log" \
    "$mock_log" \
    "$coordinator_status" \
    "$managed_status" \
    "$coordinator_doctor" \
    "$managed_doctor" \
    "$coordinator_audit" \
    "$managed_audit" <<'PY'
import json
from pathlib import Path
import sys

(
    source_log,
    target_log,
    mock_log,
    coordinator_status,
    managed_status,
    coordinator_doctor,
    managed_doctor,
    coordinator_audit,
    managed_audit,
) = map(Path, sys.argv[1:])

def jsonl(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]

source = jsonl(source_log)
target = jsonl(target_log)
mock = jsonl(mock_log)
coord_audit = jsonl(coordinator_audit)
managed_audit = jsonl(managed_audit)

assert sum(event["event"] == "remoteApproval" for event in source) == 8, source
assert all(
    event.get("actionId") == "approve-session"
    for event in source
    if event["event"] == "remoteApproval"
), source
assert any(event["event"] == "controllerPassed" for event in source), source
assert any(
    event["event"] == "itemCompleted"
    and event["payload"]
    and event["originHost"]
    and event["originThread"]
    and event["correlation"]
    for event in target
), target
assert any(event["event"] == "controllerPassed" for event in target), target
assert any(event["event"] == "targetCommandApprovalDeclined" for event in target), target

steps = {event["step"] for event in mock}
assert {
    "reviewApprove",
    "approve",
    "reviewReject",
    "reject",
    "pair",
    "grant",
    "sessionSend",
    "message",
    "targetApproval",
    "sourceFinal",
    "targetFinal",
} <= steps, mock
assert any(
    "host_pair" in event["remoteToolNames"]
    and "host_grant_set" in event["remoteToolNames"]
    and "session_message" in event["remoteToolNames"]
    and "request_review" in event["remoteToolNames"]
    and "request_approve" in event["remoteToolNames"]
    and "request_reject" in event["remoteToolNames"]
    for event in mock
), mock

for path in (coordinator_status, managed_status):
    assert json.loads(path.read_text(encoding="utf-8"))["status"] == "running"
for path in (coordinator_doctor, managed_doctor):
    assert json.loads(path.read_text(encoding="utf-8"))["ok"] is True

coord_actions = {event["action"] for event in coord_audit}
managed_actions = {event["action"] for event in managed_audit}
assert {
    "request.review",
    "request.approve",
    "request.reject",
    "pair.pending",
    "pair.completed",
    "grant.replaced",
    "message.accepted",
    "message.forwarded",
} <= coord_actions, coord_audit
assert {"pair.accepted", "grant.replaced", "message.accepted", "message.delivered"} <= managed_actions, managed_audit
assert any(
    event["actorHostId"] == "host_coord"
    and event["scope"] == "sessionMessage"
    and event["action"] == "message.delivered"
    for event in managed_audit
), managed_audit
PY
}

main() {
  require_command tmux
  require_command "$python_bin"
  require_command rg
  [[ -f "$support" ]] || fail "missing E2E controller support: $support"
  mkdir -p \
    "$outside_dir" \
    "$artifacts" \
    "$coordinator_home" \
    "$managed_home" \
    "$coordinator_workspace" \
    "$managed_workspace"
  [[ "$(cd -- "$outside_dir" && pwd -P)" != "$repo_root"* ]] ||
    fail "E2E process cwd must be outside the checkout"
  exercise_installed_lifecycle
  prepare_package

  local coordinator_peer_port managed_peer_port
  coordinator_peer_port="$(free_port)"
  managed_peer_port="$(free_port)"

  env -u PYTHONPATH "$package_agent" certificate export \
    --xedoc-home "$coordinator_home" --host-id host_coord >"$coordinator_certificate"
  env -u PYTHONPATH "$package_agent" certificate export \
    --xedoc-home "$managed_home" --host-id host_managed >"$managed_certificate"
  grep -q -- 'BEGIN CERTIFICATE' "$coordinator_certificate" ||
    fail "coordinator certificate export was not public PEM"
  grep -q -- 'BEGIN CERTIFICATE' "$managed_certificate" ||
    fail "managed certificate export was not public PEM"
  ! grep -q -- 'PRIVATE KEY' "$coordinator_certificate" ||
    fail "coordinator certificate export exposed private key material"
  ! grep -q -- 'PRIVATE KEY' "$managed_certificate" ||
    fail "managed certificate export exposed private key material"

  "$python_bin" "$support" mock \
    --port-file "$mock_port_file" \
    --request-log "$mock_log" \
    --target-thread-file "$target_ready" \
    >"$artifacts/mock.stdout.log" 2>"$artifacts/mock.stderr.log" &
  mock_pid="$!"
  wait_for_file "$mock_port_file"
  local mock_port
  mock_port="$(<"$mock_port_file")"

  write_config \
    coordinator \
    "$coordinator_home" \
    "$coordinator_workspace" \
    "tls://127.0.0.1:$coordinator_peer_port" \
    "tls://127.0.0.1:$managed_peer_port" \
    "$managed_certificate" \
    "" \
    "$mock_port"
  write_config \
    managed \
    "$managed_home" \
    "$managed_workspace" \
    "tls://127.0.0.1:$managed_peer_port" \
    "tls://127.0.0.1:$coordinator_peer_port" \
    "$coordinator_certificate" \
    "\"$coordinator_certificate\"" \
    "$mock_port"
  write_bootstrap "$coordinator_bootstrap" "$coordinator_socket"
  write_bootstrap "$managed_bootstrap" "$managed_socket"

  local coordinator_app_launcher managed_app_launcher
  coordinator_app_launcher="$(write_launcher coordinator-app-server \
    env XEDOC_HOME="$coordinator_home" RUST_LOG=xedoc_app_server=info \
    "$package_xedoc" app-server --listen "unix://$coordinator_socket")"
  managed_app_launcher="$(write_launcher managed-app-server \
    env XEDOC_HOME="$managed_home" RUST_LOG=xedoc_app_server=info \
    "$package_xedoc" app-server --listen "unix://$managed_socket")"
  tmux_session="xedoc-remote-agent-e2e-$RANDOM-$$"
  tmux new-session -d -x 240 -y 60 -s "$tmux_session" "$coordinator_app_launcher"
  tmux set-option -t "$tmux_session":0 remain-on-exit on
  tmux split-window -t "$tmux_session":0 -h "$managed_app_launcher"
  tmux select-pane -t "$tmux_session":0.0 -T coordinator-app-server
  tmux select-pane -t "$tmux_session":0.1 -T managed-app-server
  wait_for_unix_listener "$coordinator_socket"
  wait_for_unix_listener "$managed_socket"

  local coordinator_daemon_launcher managed_daemon_launcher
  coordinator_daemon_launcher="$(write_launcher coordinator-daemon \
    env XEDOC_HOME="$coordinator_home" \
    "$package_agent" serve --xedoc-home "$coordinator_home" --host-id host_coord)"
  managed_daemon_launcher="$(write_launcher managed-daemon \
    env XEDOC_HOME="$managed_home" \
    "$package_agent" serve --xedoc-home "$managed_home" --host-id host_managed)"
  tmux split-window -t "$tmux_session":0.0 -v "$coordinator_daemon_launcher"
  tmux split-window -t "$tmux_session":0.1 -v "$managed_daemon_launcher"
  tmux select-pane -t "$tmux_session":0.2 -T coordinator-daemon
  tmux select-pane -t "$tmux_session":0.3 -T managed-daemon

  local daemons_running=0
  for _ in $(seq 1 600); do
    if env -u PYTHONPATH "$package_agent" status --xedoc-home "$coordinator_home" \
      >"$coordinator_status" 2>"$artifacts/coordinator-status.stderr.log" &&
      env -u PYTHONPATH "$package_agent" status --xedoc-home "$managed_home" \
        >"$managed_status" 2>"$artifacts/managed-status.stderr.log"
    then
      if "$python_bin" - "$coordinator_status" "$managed_status" <<'PY'
import json
import sys
raise SystemExit(
    0
    if all(json.load(open(path, encoding="utf-8"))["status"] == "running" for path in sys.argv[1:])
    else 1
)
PY
      then
        daemons_running=1
        break
      fi
    fi
    sleep 0.05
  done
  [[ "$daemons_running" == 1 ]] || fail "remote-agent daemons did not report running status"
  assert_binding_provenance "$coordinator_home" ||
    fail "broker capability accepted a spoofed extension binding"

  (
    cd -- "$outside_dir"
    exec env -u PYTHONPATH "$python_bin" "$support" controller \
      --role target \
      --socket "$managed_socket" \
      --cwd "$managed_workspace" \
      --ready-file "$target_ready" \
      --timeout 60 \
      --log "$target_log"
  ) >"$artifacts/target-controller.stdout.log" 2>"$artifacts/target-controller.stderr.log" &
  target_controller_pid="$!"
  wait_for_file "$target_ready"
  (
    cd -- "$outside_dir"
    exec env -u PYTHONPATH "$python_bin" "$support" controller \
      --role source \
      --socket "$coordinator_socket" \
      --cwd "$coordinator_workspace" \
      --ready-file "$source_ready" \
      --timeout 60 \
      --log "$source_log"
  ) >"$artifacts/source-controller.stdout.log" 2>"$artifacts/source-controller.stderr.log" &
  source_controller_pid="$!"
  wait_for_process "$source_controller_pid" "source controller"
  source_controller_pid=""
  wait_for_process "$target_controller_pid" "target controller"
  target_controller_pid=""

  env -u PYTHONPATH "$package_agent" doctor --xedoc-home "$coordinator_home" \
    >"$coordinator_doctor" 2>"$artifacts/coordinator-doctor.stderr.log" ||
    fail "coordinator doctor failed"
  env -u PYTHONPATH "$package_agent" doctor --xedoc-home "$managed_home" \
    >"$managed_doctor" 2>"$artifacts/managed-doctor.stderr.log" ||
    fail "managed doctor failed"
  env -u PYTHONPATH "$package_agent" status --xedoc-home "$coordinator_home" \
    >"$coordinator_status"
  env -u PYTHONPATH "$package_agent" status --xedoc-home "$managed_home" \
    >"$managed_status"
  env -u PYTHONPATH "$package_agent" audit export --xedoc-home "$coordinator_home" \
    >"$coordinator_audit"
  env -u PYTHONPATH "$package_agent" audit export --xedoc-home "$managed_home" \
    >"$managed_audit"
  assert_evidence || fail "E2E assertions failed"

  env -u PYTHONPATH "$package_agent" shutdown --xedoc-home "$coordinator_home" \
    >"$artifacts/coordinator-shutdown.json"
  env -u PYTHONPATH "$package_agent" shutdown --xedoc-home "$managed_home" \
    >"$artifacts/managed-shutdown.json"
  for _ in $(seq 1 600); do
    env -u PYTHONPATH "$package_agent" status --xedoc-home "$coordinator_home" \
      >"$coordinator_status" 2>/dev/null || true
    env -u PYTHONPATH "$package_agent" status --xedoc-home "$managed_home" \
      >"$managed_status" 2>/dev/null || true
    if "$python_bin" - "$coordinator_status" "$managed_status" <<'PY'
import json
import sys
try:
    states = [json.load(open(path, encoding="utf-8"))["status"] for path in sys.argv[1:]]
except (OSError, KeyError, json.JSONDecodeError):
    raise SystemExit(1)
raise SystemExit(0 if states == ["stopped", "stopped"] else 1)
PY
    then
      printf '%s\n' 'PASS: packaged remote-agent two-host tmux E2E'
      return
    fi
    sleep 0.05
  done
  fail "remote-agent graceful shutdown did not complete"
}

main "$@"
