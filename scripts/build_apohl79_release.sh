#!/usr/bin/env bash

export APPLE_CODESIGN_IDENTITY="Developer ID Application: YOUR NAME (TEAMID)"

python3 scripts/build_apohl79_release.py \
  --ref main-fork \
  --target aarch64-apple-darwin \
  --base-version 0.140.0-alpha.10 \
  --archive-output dist/apohl79/0.140.0-alpha.10-apohl79/codex-aarch64-apple-darwin-0.140.0-alpha.10-apohl79.zip \
  --force
