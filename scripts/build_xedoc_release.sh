#!/usr/bin/env bash

build_args=(
  --ref main
)

has_target=false
for arg in "$@"; do
  case "$arg" in
    --target|--target=*)
      has_target=true
      break
      ;;
  esac
done

if [[ "$has_target" == false ]]; then
  build_args+=(--target macos-arm64)
fi

python3 scripts/build_xedoc_release.py \
  "${build_args[@]}" \
  --github-repo apohl79/xedoc \
  --github-account apohl79 \
  --force \
  --notarize \
  "$@"
