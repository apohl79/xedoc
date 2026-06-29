---
name: list-apohl79-fork-upstream-changes
description: List all OpenAI Codex upstream stable-release changes between the current apohl79 fork version and the latest non-alpha upstream version. Use when asked what changed upstream since the apohl79 fork version, what stable upstream release the fork is behind, whether upstream directly addresses any fork feature from README.fork.md, or to prepare an upgrade summary from the fork base to latest stable.
---

# List Apohl79 Fork Upstream Changes

## Overview

Produce a complete markdown report of upstream stable-release changes between
the current apohl79 fork version and the latest non-alpha OpenAI Codex release.
Use the bundled collector script as the source of truth.

The skill intentionally excludes alpha/prerelease tags. Stable upstream tags are
only tags matching `rust-v<major>.<minor>.<patch>`.

Do not list fork-added features or fixes as upstream changes. The main changelog
is only the upstream tag range from the fork's upstream base to latest stable.
Use `README.fork.md` only to detect whether any upstream change appears to
directly address a fork feature/fix, and report those matches in a separate
overlap section.

## Workflow

1. Start from this repository checkout and require `upstream` to point at
   `openai/codex`.
2. Run the collector:

   ```bash
   python3 .codex/skills/list-apohl79-fork-upstream-changes/scripts/list_apohl79_fork_upstream_changes.py
   ```

   The collector fetches upstream tags by default, finds the latest reachable
   fork release tag like `rust-v0.142.0-apohl79`, maps it to upstream base tag
   `rust-v0.142.0`, finds the latest stable upstream tag, and emits a markdown
   report.

3. Treat the script output as the source of truth for the answer. It includes:
   - detected fork release tag and upstream base tag,
   - latest stable upstream release,
   - every stable release between those tags,
   - upstream release notes when available from GitHub,
   - every commit in each stable release interval,
   - per-release changed-file stats,
   - separate potential upstream overlaps with `README.fork.md` fork features.

4. If the user asks for a concise answer, summarize the report but keep the
   detected tags, release range, and source commands in the response.

## Options

- `--from-tag rust-vX.Y.Z` or `--from-tag rust-vX.Y.Z-apohl79`: override the
  detected fork base.
- `--to-tag rust-vX.Y.Z`: override the detected latest stable upstream tag.
- `--no-fetch`: skip `git fetch upstream --tags --prune`.
- `--repo openai/codex`: override the GitHub repository used for release notes.
- `--upstream-remote upstream`: override the upstream git remote name.
- `--fork-readme README.fork.md`: override the fork feature/fix inventory file
  used only for overlap detection.

## Validation

Run the focused tests:

```bash
python3 .codex/skills/list-apohl79-fork-upstream-changes/scripts/test_list_apohl79_fork_upstream_changes.py
```

Smoke-test the collector from this checkout:

```bash
python3 .codex/skills/list-apohl79-fork-upstream-changes/scripts/list_apohl79_fork_upstream_changes.py --no-fetch
```
