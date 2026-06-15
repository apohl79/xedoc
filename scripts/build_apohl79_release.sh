#!/usr/bin/env bash

export APPLE_CODESIGN_IDENTITY="Developer ID Application: YOUR NAME (TEAMID)"

python3 scripts/build_apohl79_release.py \
  --ref main-fork \
  --target aarch64-apple-darwin \
  --force
