#!/usr/bin/env bash

build_args=(
  --ref main
)

has_target=false
has_resume=false
for arg in "$@"; do
  case "$arg" in
    --target|--target=*)
      has_target=true
      ;;
    --resume)
      has_resume=true
      ;;
  esac
done

if [[ "$has_target" == false ]]; then
  build_args+=(--target macos-arm64)
fi

if [[ "$has_resume" == false ]]; then
  build_args+=(--force)
fi

python3 scripts/build_xedoc_release.py \
  "${build_args[@]}" \
  --github-repo apohl79/xedoc \
  --github-account apohl79 \
  --notarize \
  "$@"
