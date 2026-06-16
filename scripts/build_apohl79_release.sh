#!/usr/bin/env bash

python3 scripts/build_apohl79_release.py \
  --ref main-fork \
  --target aarch64-apple-darwin \
  --force \
  "$@"
