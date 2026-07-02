# apohl79 Codex Fork Notes

`main-fork` is the canonical `apohl79/codex` fork branch. It tracks OpenAI
Codex while carrying fork-local features, release tooling, and fixes.

This file is the single source of truth for the fork feature/fix inventory used
by `.codex/skills/upgrade-apohl79-fork`. The upgrade skill must read this file
instead of maintaining a second feature list or a durable feature-branch list.

The inventory below lists behavior that is present on the fork and not in the
upstream release baseline being compared during a fork upgrade.

## Fork-Only Inventory

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
- Snapshot coverage verifies selected-row rendering.

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
- Custom status-line context usage aligns with `/status` output.

Primary files:

- `codex-rs/config/src/types.rs`
- `codex-rs/tui/src/bottom_pane/status_line_setup.rs`
- `codex-rs/tui/src/chatwidget/status.rs`
- `codex-rs/tui/src/chatwidget/status_surfaces.rs`
- `codex-rs/tui/src/status_line_command.rs`

### TUI Session Name Composer Label

The fork shows the current session name in the composer when a name is set.

- Named or renamed threads render the session name at the top-right of the user
  entry box.
- Empty or whitespace-only names are hidden.
- Long names are truncated with an ellipsis to preserve composer layout.
- Session-load and thread-name-update paths both keep the composer label in
  sync.
- Snapshot coverage verifies session-load rendering, rename rendering, and
  truncation.

Primary files:

- `codex-rs/tui/src/bottom_pane/chat_composer.rs`
- `codex-rs/tui/src/bottom_pane/mod.rs`
- `codex-rs/tui/src/chatwidget/session_flow.rs`
- `codex-rs/tui/src/chatwidget/tests/status_and_layout.rs`

### Automatic Session Naming

The fork can generate short session names automatically from the conversation
without changing the visible model response.

- Generated names are capped at 32 characters.
- The side-band naming task runs after completed turns and can refresh a
  generated name as the conversation changes.
- Manual `/rename` names are treated as explicit user overrides and are not
  overwritten by generated names.
- Generated names persist through the same thread metadata path as manual
  names, so session-name rendering, status-line payloads, history, and resume
  views consume them uniformly.
- The `auto_session_name` setting defaults to enabled and can be toggled with
  `/rename --auto on|off`.
- Unsafe display characters are stripped from generated names, and
  secret-shaped generated names fall back to a generic safe label.
- Integration and snapshot coverage verify generated updates, manual override
  behavior, fork behavior, setting persistence, and composer rendering.

Primary files:

- `codex-rs/core/src/session/session_name.rs`
- `codex-rs/app-server/src/auto_session_name.rs`
- `codex-rs/state/src/model/thread_metadata.rs`
- `codex-rs/thread-store/src/local/update_thread_metadata.rs`
- `codex-rs/tui/src/chatwidget/slash_dispatch.rs`
- `codex-rs/tui/src/config_update.rs`

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
  the signature, and invokes the Codex package builder with an explicit
  entrypoint and version.
- Release builds preserve incremental Cargo artifacts by using the current
  checkout and `codex-rs/target` as the default target directory.
- The helper limits default Cargo parallelism while respecting explicit caller
  settings.
- The helper can auto-repair stale workspace package versions in
  `codex-rs/Cargo.lock` before a locked release build.
- The installer targets GitHub releases in `apohl79/codex`, resolves the
  current fork tag from a checked-out tag or `[workspace.package].version`, and
  verifies the uploaded asset SHA-256 before installing.
- The release helper creates the matching GitHub release in `apohl79/codex` if
  it does not already exist.
- The release helper uploads generated archives to the release with clobbering
  enabled so rebuilding the same version replaces the previous asset.
- The release helper uses the stored `apohl79` `gh` token by default for release
  publishing, while respecting an explicit `GH_TOKEN` or `GITHUB_TOKEN`.

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
- The workflow preserves fork-only changes while rebasing or replaying
  `main-fork` onto a requested upstream release.

Primary files:

- `.codex/skills/upgrade-apohl79-fork/SKILL.md`
- `.codex/skills/upgrade-apohl79-fork/agents/openai.yaml`

### Repository Hygiene

The fork carries small repository-maintenance changes that support local
development and release hygiene.

- `.gitleaksignore` includes entries for upstream fixture secrets that can trip
  local scans.
- Generated files and TUI snapshots are refreshed after release rebases when
  upstream changes require it.
- The workspace version is pinned to the current fork release line.

Primary files:

- `.gitleaksignore`
- `codex-rs/Cargo.toml`
- `codex-rs/tui/src/**/*.snap`

## Notes For Maintainers

- Refresh this file after rebasing `main-fork` onto a newer upstream baseline.
- Feature and fix branches are short-lived staging or review branches. Do not
  maintain them as durable fork inventory once `main-fork` contains their
  commits.
- Use behavior-level summaries rather than raw commit counts.
- Keep the selected `@` picker row visually distinct. The fork's expected
  selected-row background is `Color::Rgb(55, 60, 67)`.
