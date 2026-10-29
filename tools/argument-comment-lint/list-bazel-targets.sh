#!/usr/bin/env bash

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo_root}"

# Lint production code without compiling acceptance harnesses.
production_targets="$(
  ./.github/scripts/run-bazel-query-ci.sh \
    --output=label \
    -- 'kind("rust_(library|proc_macro|binary) rule", //xedoc-rs/...) except attr(testonly, 1, //xedoc-rs/...)'
)"

# Convert semantic lint opt-outs into negative target patterns so wildcard
# builds do not analyze toolchains used only by those wrappers.
excluded_targets="$(
  ./.github/scripts/run-bazel-query-ci.sh \
    --output=label \
    -- 'attr(tags, "no-argument-comment-lint", //xedoc-rs/...)'
)"

printf '%s\n' "${production_targets}"
if [[ -n "${excluded_targets}" ]]; then
  printf '%s\n' "${excluded_targets}" | sed 's/^/-/'
fi
