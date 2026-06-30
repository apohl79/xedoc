# apohl79 Codex Fork Notes

This branch is the `apohl79/codex` fork branch `main-fork`. It tracks
OpenAI Codex while carrying fork-local features, release tooling, and fixes.

This file is the single source of truth for the fork feature/fix inventory used
by `.codex/skills/upgrade-apohl79-fork`. The upgrade skill must read this file
instead of maintaining a second feature list.

Comparison snapshot:

- Fork branch: `main-fork` at `0c8e88c8ee`
- Upstream baseline: `rust-v0.142.4` at `d0fd96663e`
- Merge base: `d0fd96663e`
- Snapshot date: 2026-06-29
- Refresh commands:
  - `git fetch upstream refs/tags/rust-v0.142.4:refs/tags/rust-v0.142.4`
  - `git log --cherry-pick --right-only rust-v0.142.4...main-fork`
  - `git diff rust-v0.142.4...main-fork`

The inventory below lists behavior that is present on the fork and not in the
upstream baseline above. It does not repeat upstream-only changes.

## Fork-Only Features

### TUI `@` File-Path Completion

The fork extends the composer `@` picker beyond the upstream project-file fuzzy
search path.

- Bare `@query` tokens still use project fuzzy search.
- Path-like tokens use direct filesystem completion, including absolute paths,
  relative paths, `./`, `../`, and `~/`.
- Directory results can be accepted with Tab to descend into that directory
  while keeping the completion popup open.
- File and directory matches are sorted with directories first.
- Stale asynchronous search results are ignored when the input query has moved
  on.
- The popup supports page up/down and keeps the active match visible when the
  result set is larger than the rendered window.
- The selected row keeps the `> ` gutter marker and uses a dark gray background:
  `Color::Rgb(55, 60, 67)`.

Primary files:

- `codex-rs/tui/src/file_search.rs`
- `codex-rs/tui/src/file_search_tests.rs`
- `codex-rs/tui/src/bottom_pane/chat_composer.rs`
- `codex-rs/tui/src/bottom_pane/file_search_popup.rs`
- `codex-rs/tui/src/bottom_pane/mentions_v2/popup.rs`
- `codex-rs/tui/src/bottom_pane/mentions_v2/render.rs`

### Custom TUI Status Line

The fork adds configurable status-line support for the TUI, including a custom
external command mode.

- `/statusline` setup can enable a built-in status line, select individual
  built-in items, toggle theme colors, or enable a custom command.
- Config keys include `status_line`, `status_line_use_colors`, and
  `status_line_command`.
- Custom commands can be configured as a shell command string or as an argv
  list.
- The setup flow defaults the command path to `~/.claude/statusline.sh`.
- The TUI runs custom status-line commands asynchronously and sends a
  Claude-compatible JSON payload on stdin.
- The payload includes Codex-specific metadata such as `harness: "codex"`,
  session name, task indicator, context-window usage, token usage, git/project
  details, model/reasoning settings, approval and sandbox status, rate-limit
  information, and current status text.

Primary files:

- `codex-rs/config/src/types.rs`
- `codex-rs/tui/src/bottom_pane/status_line_setup.rs`
- `codex-rs/tui/src/chatwidget/status.rs`
- `codex-rs/tui/src/chatwidget/status_surfaces.rs`
- `codex-rs/tui/src/status_line_command.rs`

### Persistent Active Task List

The fork keeps the current `update_plan` checklist visible in the TUI while a
task is running.

- The active checklist is rendered directly above the user entry field.
- Completed, in-progress, and pending steps use distinct markers and styling.
- Empty plan steps are ignored.
- Long task lists are capped in the bottom pane with an overflow row.
- The list clears when a task starts fresh or completes.
- Replayed plan updates are ignored unless a task is currently running.
- Snapshot and layout tests cover the rendered list and task lifecycle.

Primary files:

- `codex-rs/tui/src/bottom_pane/active_task_list.rs`
- `codex-rs/tui/src/bottom_pane/mod.rs`
- `codex-rs/tui/src/chatwidget/turn_runtime.rs`
- `codex-rs/tui/src/chatwidget/tests/status_and_layout.rs`

### apohl79 Release Packaging

The fork adds release helpers for building apohl79-branded packages from
`main-fork`.

- `scripts/build_apohl79_release.py` and
  `scripts/build_apohl79_release.sh` provide release entry points.
- `scripts/apohl79_release.py` contains the shared release implementation.
- `scripts/install/install-apohl79.sh` installs the fork binary release for the
  current `rust-v*-apohl79` tag.
- The default release ref is `main-fork`.
- The default fork suffix is `apohl79`.
- The default output directory is `dist/apohl79`.
- The release helper verifies that the current checkout matches the requested
  ref.
- The helper requires `codex-rs/Cargo.toml` and `codex-rs/Cargo.lock` to be
  clean before packaging.
- macOS package signing requires a non-placeholder Developer ID Application
  identity.
- The helper builds `codex-cli` with Cargo `--locked`, signs the binary, verifies
  the signature, and then invokes the Codex package builder with an explicit
  entrypoint and version.
- Release builds preserve incremental Cargo artifacts by using the current
  checkout and `codex-rs/target` as the default target directory.
- The helper can auto-repair stale workspace package versions in
  `codex-rs/Cargo.lock` before a locked release build.
- The installer targets GitHub releases in `apohl79/codex`, resolves the
  current fork tag from a checked-out tag or `[workspace.package].version`, and
  verifies the uploaded asset SHA-256 before installing.
- Release helper tests cover version detection, signing validation, Cargo job
  limiting, target-dir behavior, and lockfile repair.

Primary files:

- `scripts/apohl79_release.py`
- `scripts/build_apohl79_release.py`
- `scripts/build_apohl79_release.sh`
- `scripts/install/install-apohl79.sh`
- `scripts/test_apohl79_release.py`
- `scripts/codex_package/cli.py`

### Fork Upgrade Tooling

The fork includes a local Codex skill for upgrading this fork from upstream
OpenAI Codex release tags.

- The skill documents the fork upgrade workflow.
- The skill includes an `openai` subagent for upstream inspection.
- The workflow preserves fork-only changes while rebasing or replaying onto a
  requested upstream release.

Primary files:

- `.codex/skills/upgrade-apohl79-fork/SKILL.md`
- `.codex/skills/upgrade-apohl79-fork/agents/openai.yaml`

## Fork-Only Fixes and Maintenance

- Improved `@` mention popup scrolling and active-row visibility.
- Improved selected-row contrast for the `@` picker with the dark gray active
  background.
- Kept directory `@` completion active after entering a directory with Tab.
- Added snapshot coverage for the `@` mention popup selected-row rendering.
- Fixed custom status-line context usage so it aligns with `/status` output.
- Exposed status-line harness metadata for custom command payloads.
- Added session-name and task-progress metadata to status surfaces.
- Added a persistent active task list above the TUI user entry field.
- Kept apohl79 release builds incremental instead of forcing disposable build
  directories.
- Added default Cargo job limiting for apohl79 release builds.
- Added automatic apohl79 release version detection.
- Added release lockfile repair for stale workspace package versions.
- Synced generated files and TUI snapshots after the 0.142.4 fork rebase.
- Added `.gitleaksignore` entries for upstream fixture secrets that can trip
  local scans.
- Pinned the fork workspace version to `0.142.4` for the fork release line.

## Notes For Maintainers

- Refresh this file after rebasing `main-fork` onto a newer upstream baseline.
- Use behavior-level summaries rather than raw commit counts: the fork history
  contains duplicated `@` completion commits from a feature branch that was
  later merged back into `main-fork`.
- Keep the selected `@` picker row visually distinct. The fork's expected
  selected-row background is `Color::Rgb(55, 60, 67)`.
