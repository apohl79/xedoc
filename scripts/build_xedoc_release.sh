#!/usr/bin/env bash

python3 scripts/build_xedoc_release.py \
  --ref main \
  --target aarch64-apple-darwin \
  --github-repo apohl79/codex \
  --github-account apohl79 \
  --force \
  "$@"
