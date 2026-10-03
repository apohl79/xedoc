#!/usr/bin/env bash

python3 scripts/build_xedoc_release.py \
  --ref main \
  --target macos-arm64 \
  --target macos-x86_64 \
  --target linux-x86_64 \
  --target linux-arm64 \
  --github-repo apohl79/xedoc \
  --github-account apohl79 \
  --force \
  --notarize \
  "$@"
