#!/usr/bin/env bash

set -euo pipefail

bazel_lint_args=("$@")
if [[ "${RUNNER_OS:-}" == "Windows" ]]; then
  has_host_platform_override=0
  for arg in "${bazel_lint_args[@]}"; do
    if [[ "$arg" == --host_platform=* ]]; then
      has_host_platform_override=1
      break
    fi
  done

  if [[ $has_host_platform_override -eq 0 ]]; then
    # The nightly Windows lint toolchain is registered with an MSVC exec
    # platform even though the lint target platform stays on `windows-gnullvm`.
    # Override the host platform here so the exec-side helper binaries actually
    # match the registered toolchain set.
    bazel_lint_args+=("--host_platform=//:local_windows_msvc")
  fi

  # Native Windows lint runs need exec-side Rust helper binaries and proc-macros
  # to use rust-lld instead of the C++ linker path. The default `none`
  # preference resolves to `cc` when a cc_toolchain is present, which currently
  # routes these exec actions through clang++ with an argument shape it cannot
  # consume.
  bazel_lint_args+=("--@rules_rust//rust/settings:toolchain_linker_preference=rust")

  # Some Rust top-level targets are still intentionally incompatible with the
  # local Windows MSVC exec platform. Skip those explicit targets so the native
  # lint aspect can run across the compatible crate graph instead of failing the
  # whole build after analysis.
  bazel_lint_args+=("--skip_incompatible_explicit_targets")
fi

production_targets="$(./tools/argument-comment-lint/list-bazel-targets.sh)"
final_build_targets=()
while IFS= read -r label; do
  [[ -n "$label" ]] || continue
  final_build_targets+=("$label")
done <<< "${production_targets}"

if [[ ${#final_build_targets[@]} -eq 0 ]]; then
  echo "Failed to discover Bazel lint targets." >&2
  exit 1
fi

./.github/scripts/run-bazel-ci.sh \
  -- \
  build \
  "${bazel_lint_args[@]}" \
  -- \
  "${final_build_targets[@]}"
