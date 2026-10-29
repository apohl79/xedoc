#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --windows-cross-compile)
      shift
      ;;
    *)
      echo "Usage: $0 [--windows-cross-compile]" >&2
      exit 1
      ;;
  esac
done

# Resolve the dynamic targets before printing anything so callers do not
# continue with a partial list if `bazel query` fails. Target discovery is
# local on all platforms.
production_targets="$(
  ./.github/scripts/run-bazel-query-ci.sh \
    --output=label \
    -- 'kind("rust_(library|proc_macro|binary) rule", //xedoc-rs/...) except attr(testonly, 1, //xedoc-rs/...)'
)"
printf '%s\n' "${production_targets}"
