#!/usr/bin/env bash

set -euo pipefail

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

xedoc_bin="${XEDOC_TMUX_TEST_BIN:?set XEDOC_TMUX_TEST_BIN to a packaged xedoc binary}"
seed_home="${XEDOC_TMUX_TEST_SEED_HOME:-}"
prompt="${XEDOC_TMUX_TEST_PROMPT:-Provide a concise weather report for Neuenhagen bei Berlin this week; use a subagent.}"
router_mode="${XEDOC_TMUX_TEST_ROUTER_MODE:-full}"
router_approval="${XEDOC_TMUX_TEST_ROUTER_APPROVAL:-all}"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/xedoc-model-router-tmux.XXXXXX")"
test_home="$tmp_dir/home"
socket_path="$test_home/app-server.sock"
server_session="xedoc-router-server-$$"
client_session="xedoc-router-client-$$"
open_override="${XEDOC_TMUX_TEST_OPEN_OVERRIDE:-0}"
override_keys="${XEDOC_TMUX_TEST_OVERRIDE_KEYS:-}"
keep_tmp_dir="${XEDOC_TMUX_TEST_KEEP_DIR:-0}"
capture_delay_seconds="${XEDOC_TMUX_TEST_CAPTURE_DELAY_SECONDS:-10}"
expect_approval="${XEDOC_TMUX_TEST_EXPECT_APPROVAL:-}"
expect_preselected="${XEDOC_TMUX_TEST_EXPECT_PRESELECTED:-}"

cleanup() {
  tmux send-keys -t "$server_session":0.0 C-c >/dev/null 2>&1 || true
  sleep 1
  tmux kill-session -t "$client_session" >/dev/null 2>&1 || true
  tmux kill-session -t "$server_session" >/dev/null 2>&1 || true
  server_pid="$(pgrep -f "app-server.*$socket_path" || true)"
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" >/dev/null 2>&1 || true
  fi
  sleep 1
  if [[ "$keep_tmp_dir" == "1" ]]; then
    printf 'Retained XEDOC_HOME: %s\n' "$test_home"
  else
    rm -rf "$tmp_dir"
  fi
}

trap cleanup EXIT INT TERM HUP

if [[ ! -x "$xedoc_bin" ]]; then
  echo "XEDOC_TMUX_TEST_BIN is not executable: $xedoc_bin" >&2
  exit 1
fi

mkdir -p "$test_home"
if [[ -n "$seed_home" ]]; then
  for file in auth.json config.toml model-router.toml models.json; do
    if [[ -f "$seed_home/$file" ]]; then
      cp "$seed_home/$file" "$test_home/$file"
    fi
  done
fi

server_args=(
  env
  "XEDOC_HOME=$test_home"
  XEDOC_DISABLE_AUTO_UPDATE=1
  "$xedoc_bin"
  --enable
  model_router
  -c
  "model_router.mode=\"$router_mode\""
  -c
  "model_router.approval=\"$router_approval\""
  app-server
  --listen
  "unix://$socket_path"
)
printf -v server_command '%q ' "${server_args[@]}"
tmux new-session -d -s "$server_session" "$server_command"

for _ in $(seq 1 100); do
  if [[ -S "$socket_path" ]]; then
    break
  fi
  sleep 0.05
done

if [[ ! -S "$socket_path" ]]; then
  tmux capture-pane -pt "$server_session":0.0 -S -100 >&2 || true
  echo "timed out waiting for dedicated app-server socket: $socket_path" >&2
  exit 1
fi

client_args=(
  env
  "XEDOC_HOME=$test_home"
  XEDOC_DISABLE_AUTO_UPDATE=1
  "$xedoc_bin"
  --remote
  "unix://$socket_path"
  --no-alt-screen
  "$prompt"
)
printf -v client_command '%q ' "${client_args[@]}"
tmux new-session -d -s "$client_session" "$client_command"

if [[ "${XEDOC_TMUX_TEST_INTERACTIVE:-0}" == "1" ]]; then
  tmux attach-session -t "$client_session"
else
  sleep "$capture_delay_seconds"
  if [[ "$open_override" == "1" ]]; then
    tmux send-keys -t "$client_session":0.0 o
    sleep 1
    for key in $override_keys; do
      tmux send-keys -t "$client_session":0.0 "$key"
    done
    if [[ -n "$override_keys" ]]; then
      sleep 2
    fi
  fi
  pane="$(tmux capture-pane -pt "$client_session":0.0 -S -160)"
  printf '%s\n' "$pane"
  case "$expect_approval" in
    yes)
      if [[ "$pane" != *"Model route approval"* ]]; then
        echo "expected a model-route approval prompt" >&2
        exit 1
      fi
      ;;
    no)
      if [[ "$pane" == *"Model route approval"* ]]; then
        echo "shadow mode unexpectedly requested model-route approval" >&2
        exit 1
      fi
      ;;
    "")
      ;;
    *)
      echo "XEDOC_TMUX_TEST_EXPECT_APPROVAL must be yes or no" >&2
      exit 1
      ;;
  esac
  if [[ -n "$expect_preselected" && "$pane" != *"$expect_preselected"* ]]; then
    echo "expected preselected override value was not rendered: $expect_preselected" >&2
    exit 1
  fi
fi
