#!/usr/bin/env bash

# Run the complete host-native validation matrix used for fork upgrade checkpoints.
#
# This intentionally runs every stage even after failures so its log is a complete
# diagnostic record. Cross-platform CI matrix legs still require their native runners.

set -uo pipefail
set +e

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
codex_rs_root="${repo_root}/codex-rs"
failures=()

configure_testcontainers() {
  local colima_socket="${HOME}/.colima/default/docker.sock"

  if [[ -S "${colima_socket}" ]]; then
    export DOCKER_HOST="unix://${colima_socket}"
  fi
  if [[ -n "${DOCKER_HOST:-}" && -z "${TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE:-}" ]]; then
    export TESTCONTAINERS_DOCKER_SOCKET_OVERRIDE=/var/run/docker.sock
  fi
}

verify_argument_comment_lint_targets() {
  "${repo_root}/tools/argument-comment-lint/list-bazel-targets.sh" >/dev/null
}

run_stage() {
  local name="$1"
  local directory="$2"
  local started_at
  local status
  shift 2

  started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '\n==> [%s] %s\n' "${started_at}" "${name}"
  printf '$ (cd %q &&' "${directory}"
  printf ' %q' "$@"
  printf ' )\n'

  if (
    cd "${directory}"
    "$@"
  ); then
    printf '<== %s: passed\n' "${name}"
    return 0
  else
    status=$?
    failures+=("${name} (exit ${status})")
    printf '<== %s: failed (exit %s)\n' "${name}" "${status}"
  fi
  return 0
}

require_clean_worktree() {
  if [[ -n "$(git -C "${repo_root}" status --porcelain)" ]]; then
    echo "Validation requires a clean worktree." >&2
    git -C "${repo_root}" status --short >&2
    return 1
  fi
}

main() {
  if ! require_clean_worktree; then
    return 2
  fi

  configure_testcontainers
  printf 'Repository: %s\n' "${repo_root}"
  printf 'Revision: %s\n' "$(git -C "${repo_root}" rev-parse HEAD)"
  printf 'DOCKER_HOST: %s\n' "${DOCKER_HOST:-<unset>}"

  run_stage "repository formatting" "${repo_root}" just fmt-check
  run_stage "repository structure checks" "${repo_root}" python3 .github/scripts/verify_cargo_workspace_manifests.py
  run_stage "TUI/core boundary check" "${repo_root}" python3 .github/scripts/verify_tui_core_boundary.py
  run_stage "Bazel/Cargo clippy configuration check" "${repo_root}" python3 .github/scripts/verify_bazel_clippy_lints.py
  run_stage "Node dependency installation" "${repo_root}" pnpm install --frozen-lockfile
  run_stage "Prettier formatting check" "${repo_root}" pnpm run format

  run_stage "Cargo workspace compile" "${codex_rs_root}" cargo check --workspace --all-targets
  run_stage "Cargo workspace clippy" "${codex_rs_root}" just clippy --workspace --all-targets -- -D warnings
  run_stage "Cargo dependency lint" "${codex_rs_root}" cargo shear --deny-warnings
  run_stage "Bazel lockfile check" "${repo_root}" just bazel-lock-check
  run_stage "argument-comment lint target discovery" "${repo_root}" verify_argument_comment_lint_targets
  run_stage "argument-comment lint" "${repo_root}" just argument-comment-lint
  run_stage "Bazel clippy" "${repo_root}" just bazel-clippy

  run_stage "Cargo workspace tests" "${codex_rs_root}" just test
  run_stage "Bazel tests" "${repo_root}" just bazel-test
  run_stage "Rust benchmark smoke test" "${codex_rs_root}" just bench-smoke
  run_stage "release binary compile" "${repo_root}" just build-for-release

  run_stage "GitHub script tests" "${repo_root}" just test-github-scripts
  run_stage "package-builder tests" "${repo_root}" python3 -m unittest discover -s scripts/codex_package -p 'test_*.py'
  run_stage "installer tests" "${repo_root}" python3 -m unittest discover -s scripts/install -p 'test_*.py'
  run_stage "fork release-helper tests" "${repo_root}" uv run --frozen --project scripts python -m unittest discover -s scripts -p 'test_apohl79*_release.py'
  run_stage "argument-comment Python compilation" "${repo_root}" python3 -m py_compile tools/argument-comment-lint/wrapper_common.py tools/argument-comment-lint/run.py tools/argument-comment-lint/run-prebuilt-linter.py tools/argument-comment-lint/test_wrapper_common.py
  run_stage "argument-comment Python tests" "${repo_root}" python3 -m unittest discover -s tools/argument-comment-lint -p 'test_*.py'
  run_stage "argument-comment Rust tests" "${repo_root}/tools/argument-comment-lint" env RUST_MIN_STACK=8388608 cargo test

  run_stage "clean worktree after validation" "${repo_root}" git diff --exit-code
  if [[ -n "$(git -C "${repo_root}" status --porcelain)" ]]; then
    failures+=("clean worktree after validation (untracked or staged files)")
    git -C "${repo_root}" status --short
  fi

  printf '\n===== Validation summary =====\n'
  if [[ ${#failures[@]} -eq 0 ]]; then
    echo 'All host-native validation stages passed.'
    return 0
  fi

  printf '%s validation stage(s) failed:\n' "${#failures[@]}"
  printf '  - %s\n' "${failures[@]}"
  return 1
}

main "$@"
