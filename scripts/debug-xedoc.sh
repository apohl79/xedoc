#!/bin/bash

# Set "chatgpt.cliExecutable": "/Users/<USERNAME>/code/xedoc/scripts/debug-xedoc.sh" in VSCode settings to always get the 
# latest xedoc-rs binary when debugging Xedoc Extension.


set -euo pipefail

XEDOC_RS_DIR=$(realpath "$(dirname "$0")/../xedoc-rs")
(cd "$XEDOC_RS_DIR" && cargo run --quiet --bin xedoc -- "$@")