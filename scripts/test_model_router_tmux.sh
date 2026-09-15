#!/usr/bin/env bash

set -euo pipefail

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is required" >&2
  exit 1
fi

xedoc_bin="${XEDOC_TMUX_TEST_BIN:?set XEDOC_TMUX_TEST_BIN to a packaged xedoc binary}"
seed_home="${XEDOC_TMUX_TEST_SEED_HOME:-}"
prompt="${XEDOC_TMUX_TEST_PROMPT:-What is 2 + 2?}"
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
open_policy_baseline="${XEDOC_TMUX_TEST_OPEN_POLICY_BASELINE:-0}"
policy_baseline_index="${XEDOC_TMUX_TEST_POLICY_BASELINE_INDEX:-1}"
expect_policy_baseline="${XEDOC_TMUX_TEST_EXPECT_POLICY_BASELINE:-}"
assert_all_mappings="${ROUTER_TMUX_TEST_ASSERT_ALL_MAPPINGS:-1}"
approval_was_resolved=0

cleanup() {
  if [[ "$keep_tmp_dir" == "1" ]]; then
    tmux capture-pane -pt "$server_session":0.0 -S -1000 > "$test_home/app-server-pane.log" \
      || true
  fi
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

capture_pane() {
  tmux capture-pane -pt "$client_session":0.0 -S -160
}

wait_for_pane_text() {
  local expected="$1"
  local attempts="${2:-100}"
  local pane
  for _ in $(seq 1 "$attempts"); do
    pane="$(capture_pane)"
    if [[ "$pane" == *"$expected"* ]]; then
      return
    fi
    sleep 0.1
  done
  printf '%s\n' "$pane" >&2
  echo "timed out waiting for client text: $expected" >&2
  exit 1
}

wait_for_pane_text_to_disappear() {
  local expected="$1"
  local attempts="${2:-100}"
  local pane
  for _ in $(seq 1 "$attempts"); do
    pane="$(capture_pane)"
    if [[ "$pane" != *"$expected"* ]]; then
      return
    fi
    sleep 0.1
  done
  printf '%s\n' "$pane" >&2
  echo "timed out waiting for client text to disappear: $expected" >&2
  exit 1
}

wait_for_client_ready() {
  local pane
  for _ in $(seq 1 300); do
    pane="$(tmux capture-pane -pt "$client_session":0.0)"
    if [[ "$pane" == *"╭─ XEDOC"* && "$pane" != *"loading"* ]]; then
      return
    fi
    sleep 0.1
  done
  printf '%s\n' "$pane" >&2
  echo "timed out waiting for the remote client to become ready" >&2
  exit 1
}

set_override_axis() {
  local axis="$1"
  local value="$2"
  local axis_index
  case "$axis" in
    work_type) axis_index=0 ;;
    complexity) axis_index=1 ;;
    orchestration) axis_index=2 ;;
    risk) axis_index=3 ;;
    *)
      echo "unknown model-router axis: $axis" >&2
      exit 1
      ;;
  esac
  while [[ "$override_selected_axis" -ne "$axis_index" ]]; do
    tmux send-keys -t "$client_session":0.0 Down
    override_selected_axis=$(( (override_selected_axis + 1) % 4 ))
    sleep 0.05
  done
  for _ in $(seq 1 20); do
    pane="$(capture_pane)"
    if [[ "$pane" == *"> $axis: $value"* ]]; then
      return
    fi
    tmux send-keys -t "$client_session":0.0 Right
    sleep 0.05
  done
  printf '%s\n' "$(capture_pane)" >&2
  echo "could not select $axis=$value in model-router approval" >&2
  exit 1
}

assert_override_route() {
  local expected_route="$1"
  local expected_calculation="$2"
  pane="$(capture_pane)"
  if [[ "$pane" != *"route: $expected_route (derived)"* ]]; then
    printf '%s\n' "$pane" >&2
    echo "expected derived route: $expected_route" >&2
    exit 1
  fi
  if [[ "$pane" != *"$expected_calculation"* ]]; then
    printf '%s\n' "$pane" >&2
    echo "expected routing calculation: $expected_calculation" >&2
    exit 1
  fi
}

write_mapping_matrix() {
  python3 - "$test_home/model-router.toml" <<'PY'
import itertools
import sys
import tomllib

policy = tomllib.load(open(sys.argv[1], "rb"))
class_order = {"simple": 0, "smart": 1, "intelligent": 2}
axes = {axis["id"]: axis["classes"] for axis in policy["axes"]}
work_groups = {}
for item in axes["work_type"]:
    if item["id"] == "steering":
        continue
    work_groups.setdefault(
        (item["points"], item["minimum_model_class"], item["maximum_model_class"]),
        [],
    ).append(item["id"])
work_values = [
    {
        "id": f"group{index}: {', '.join(items)}",
        "points": points,
        "minimum_model_class": minimum,
        "maximum_model_class": maximum,
    }
    for index, ((points, minimum, maximum), items) in enumerate(sorted(work_groups.items()))
]
axis_values = {
    "work_type": work_values,
    **{axis: values for axis, values in axes.items() if axis != "work_type"},
}
ladder = sorted(policy["ranking"]["ladder"], key=lambda entry: entry["rank"])
minimum_score = policy["ranking"]["minimum_score"]
maximum_score = policy["ranking"]["maximum_score"]
domain = maximum_score - minimum_score
for values in itertools.product(
    axis_values["work_type"],
    axis_values["complexity"],
    axis_values["orchestration"],
    axis_values["risk"],
):
    score = sum(value["points"] for value in values)
    minimum = max(
        (value["minimum_model_class"] for value in values),
        key=class_order.__getitem__,
    )
    maximum = max(
        (value["maximum_model_class"] for value in values),
        key=class_order.__getitem__,
    )
    eligible = [
        entry for entry in ladder
        if class_order[minimum] <= class_order[entry["class"]] <= class_order[maximum]
    ]
    min_rank = eligible[0]["rank"]
    max_rank = eligible[-1]["rank"]
    bounded = max(minimum_score, min(maximum_score, score))
    offset = 0 if domain == 0 else (
        (bounded - minimum_score) * (max_rank - min_rank) + domain // 2
    ) // domain
    target = min_rank + offset
    selected = min(eligible, key=lambda entry: abs(entry["rank"] - target))
    route = f'{selected["provider"]}/{selected["model"]}/{selected["reasoning_effort"]}'
    calculation = (
        f"score {score} (bounded {bounded}; domain {minimum_score}–{maximum_score})"
        f" · classes {minimum}–{maximum} · ranks {min_rank}–{max_rank}"
        f" · target {target} · selected {selected['rank']}"
    )
    print("\t".join([*(value["id"] for value in values), route, calculation]))
PY
}

configure_router_mode() {
  python3 - "$test_home/config.toml" "$router_mode" "$router_approval" <<'PY'
import pathlib
import re
import sys

path = pathlib.Path(sys.argv[1])
mode, approval = sys.argv[2:]
text = path.read_text() if path.exists() else ""
section = re.compile(r"(?ms)^\[model_router\]\n.*?(?=^\[|\Z)")
match = section.search(text)
if match:
    body = match.group()
    body = re.sub(r"(?m)^mode\s*=.*$", f'mode = "{mode}"', body)
    body = re.sub(r"(?m)^approval\s*=.*$", f'approval = "{approval}"', body)
    if not re.search(r"(?m)^mode\s*=", body):
        body += f'mode = "{mode}"\n'
    if not re.search(r"(?m)^approval\s*=", body):
        body += f'approval = "{approval}"\n'
    text = text[:match.start()] + body + text[match.end():]
else:
    text += f'\n[model_router]\nmode = "{mode}"\napproval = "{approval}"\n'
path.write_text(text)
PY
}

if [[ ! -x "$xedoc_bin" ]]; then
  echo "XEDOC_TMUX_TEST_BIN is not executable: $xedoc_bin" >&2
  exit 1
fi

tmux start-server
mkdir -p "$test_home"
if [[ -n "$seed_home" ]]; then
  for file in auth.json config.toml model-router.toml models.json; do
    if [[ -f "$seed_home/$file" ]]; then
      cp "$seed_home/$file" "$test_home/$file"
    fi
  done
fi
configure_router_mode

server_args=(
  env
  -u
  XEDOC_TMUX_TEST_ASSERT_ALL_MAPPINGS
  -u
  XEDOC_TMUX_TEST_OPEN_OVERRIDE
  -u
  ROUTER_TMUX_TEST_ASSERT_ALL_MAPPINGS
  "XEDOC_HOME=$test_home"
  XEDOC_DISABLE_AUTO_UPDATE=1
  "RUST_LOG=${RUST_LOG:-}"
  "$xedoc_bin"
  --enable
  model_router
  app-server
  --listen
  "unix://$socket_path"
)
printf -v server_command '%q ' "${server_args[@]}"
if [[ "$keep_tmp_dir" == "1" ]]; then
  printf -v server_log_path '%q' "$test_home/app-server.log"
  server_command+=" >$server_log_path 2>&1"
fi
tmux new-session -d -x 240 -y 60 -s "$server_session" "$server_command"

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
  -u
  XEDOC_TMUX_TEST_ASSERT_ALL_MAPPINGS
  -u
  XEDOC_TMUX_TEST_OPEN_OVERRIDE
  -u
  ROUTER_TMUX_TEST_ASSERT_ALL_MAPPINGS
  "XEDOC_HOME=$test_home"
  XEDOC_DISABLE_AUTO_UPDATE=1
  "RUST_LOG=${RUST_LOG:-}"
  "$xedoc_bin"
  --enable
  model_router
  --no-alt-screen
)
client_args+=(
  --remote
  "unix://$socket_path"
)
printf -v client_command '%q ' "${client_args[@]}"
tmux new-session -d -x 240 -y 60 -s "$client_session" "$client_command"

if [[ "${XEDOC_TMUX_TEST_INTERACTIVE:-0}" == "1" ]]; then
  tmux attach-session -t "$client_session"
else
  wait_for_client_ready
  sleep "$capture_delay_seconds"
  if [[ "$open_policy_baseline" != "1" ]]; then
    tmux send-keys -t "$client_session":0.0 -l "$prompt"
    wait_for_pane_text "❯ $prompt"
    tmux send-keys -t "$client_session":0.0 Enter
    sleep 3
  fi
  if [[ "$open_policy_baseline" == "1" ]]; then
    tmux send-keys -t "$client_session":0.0 "/model-router" Enter
    sleep 3
    for _ in $(seq 1 6); do
      tmux send-keys -t "$client_session":0.0 j
      sleep 0.5
    done
    tmux send-keys -t "$client_session":0.0 Enter
    sleep 1
    for _ in $(seq 1 3); do
      tmux send-keys -t "$client_session":0.0 j
      sleep 0.5
    done
    tmux send-keys -t "$client_session":0.0 Enter
    sleep 1
    for _ in $(seq 0 "$policy_baseline_index"); do
      tmux send-keys -t "$client_session":0.0 j
      sleep 0.5
    done
    tmux send-keys -t "$client_session":0.0 Enter
    sleep 2
  fi
  if [[ "$open_override" == "1" ]]; then
    wait_for_pane_text "Model route approval"
    tmux send-keys -t "$client_session":0.0 o
    wait_for_pane_text "The route is derived."
    override_selected_axis=0
    if [[ "$assert_all_mappings" == "1" ]]; then
      while IFS=$'\t' read -r work_type complexity orchestration risk expected_route expected_calculation; do
        set_override_axis work_type "$work_type"
        set_override_axis complexity "$complexity"
        set_override_axis orchestration "$orchestration"
        set_override_axis risk "$risk"
        assert_override_route "$expected_route" "$expected_calculation"
      done < <(write_mapping_matrix)
      set_override_axis work_type "group0: question, docs_analysis, packaging, operational, testing"
      set_override_axis complexity low
      set_override_axis orchestration none
      set_override_axis risk low
      assert_override_route \
        "openai/gpt-5.6-luna/low" \
        "score 3 (bounded 3; domain 3–35) · classes simple–smart · ranks 1–10 · target 1 · selected 1"
      tmux send-keys -t "$client_session":0.0 Enter
      wait_for_pane_text_to_disappear "The route is derived." 200
      approval_was_resolved=1
    else
      for key in $override_keys; do
        tmux send-keys -t "$client_session":0.0 "$key"
      done
      if [[ -n "$override_keys" ]]; then
        sleep 2
      fi
    fi
  fi
  if [[ "$expect_approval" == "yes" && "$approval_was_resolved" == "0" ]]; then
    wait_for_pane_text "Model route approval" 200
  fi
  pane="$(capture_pane)"
  printf '%s\n' "$pane"
  case "$expect_approval" in
    yes)
      if [[ "$approval_was_resolved" == "0" && "$pane" != *"Model route approval"* ]]; then
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
  if [[ -n "$expect_policy_baseline" ]]; then
    if ! rg -F "reporting_baseline" "$test_home/model-router.toml" >/dev/null \
      || ! rg -F "$expect_policy_baseline" "$test_home/model-router.toml" >/dev/null; then
      echo "expected reporting baseline was not persisted: $expect_policy_baseline" >&2
      exit 1
    fi
  fi
fi
