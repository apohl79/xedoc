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
- The built-in status line is hidden while a custom command is pending so stale
  built-in content does not flash as a fallback.
- Custom command failures and status-line errors are surfaced instead of being
  silently replaced.
- Status-line output is preserved across TUI clear/rebuild flows when the
  configured command has not changed.

Primary files:

- `codex-rs/config/src/types.rs`
- `codex-rs/tui/src/bottom_pane/status_line_setup.rs`
- `codex-rs/tui/src/chatwidget/status_controls.rs`
- `codex-rs/tui/src/chatwidget/status.rs`
- `codex-rs/tui/src/chatwidget/status_surfaces.rs`
- `codex-rs/tui/src/status_line_command.rs`

### TUI Session Name Composer Label

The fork shows the current session name as a `[Name]` title on the composer
border when a name is set.

- Named or renamed threads render the session name as a bracket-enclosed title
  on the rounded composer border. Brackets use the border color; the name text
  uses the session-name accent color.
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

### City Lights (Doom Emacs) Color Theme

The fork ships a bundled City Lights syntax highlighting theme and remaps all
TUI chrome colors to the City Lights palette.

- `[tui] theme = "city-lights"` selects the bundled syntax theme.
- The embedded `city-lights.tmTheme` ships in the binary and is listed in the
  `/theme` picker.
- A `CityLightsStylize` extension trait replaces ratatui ANSI color chaining:
  `.cl_cyan()` → `#008B94`, `.cl_green()` → `#5CD6B6`,
  `.cl_red()` → `#D95468`, `.cl_magenta()` → `#A06BEA`.
- The composer uses a full rounded-corner border with the session name
  rendered as a `[Name]` title. Brackets use the border color and the text
  uses the session-name accent color.
- The composer prompt uses `❯` in City Lights magenta (`.cl_magenta()`).
- Chat history user messages use `❯` as the prefix in City Lights magenta.
- Diff backgrounds, accents, and status-line theme styles all derive from the
  City Lights palette constants.

Primary files:

- `codex-rs/tui/src/city_lights.rs`
- `codex-rs/tui/src/style.rs`
- `codex-rs/tui/src/render/highlight.rs`
- `codex-rs/tui/src/render/themes/city-lights.tmTheme`
- `codex-rs/tui/src/bottom_pane/chat_composer.rs`
- `codex-rs/tui/src/history_cell/messages.rs`


The fork can generate short session names automatically from the conversation
without changing the visible model response.

- Generated names are capped at 32 characters.
- The side-band naming task runs after completed turns and can refresh a
  generated name as the conversation changes.
- App-server sessions can request a generated name mid-turn from early
  streaming assistant text.
- Manual `/rename` names are treated as explicit user overrides and are not
  overwritten by generated names.
- Generated names persist through the same thread metadata path as manual
  names, so session-name rendering, status-line payloads, history, and resume
  views consume them uniformly.
- Thread title metadata records whether a name is derived, generated, or set by
  the user, and app-server `thread/name/updated` notifications expose that
  source.
- The `auto_session_name` setting defaults to enabled and can be toggled with
  `/rename --auto on|off`.
- The optional `model_fast` setting can select the cheaper side-band model for
  non-OpenAI providers. OpenAI providers prefer an available `mini` model.
- Unsafe display characters are stripped from generated names, and
  secret-shaped generated names fall back to a generic safe label.
- Generated names preserve whole words, trim repeated title prefixes, handle
  malformed model output, and accumulate both final message items and
  `response.output_text.delta` chunks.
- Integration and snapshot coverage verify generated updates, manual override
  behavior, fork behavior, setting persistence, and composer rendering.

Primary files:

- `codex-rs/core/src/session/session_name.rs`
- `codex-rs/app-server/src/auto_session_name.rs`
- `codex-rs/app-server/src/thread_state.rs`
- `codex-rs/app-server-protocol/src/protocol/v2/thread.rs`
- `codex-rs/state/migrations/0040_threads_title_source.sql`
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

### Active Agent and Thread Context UI

The fork keeps multi-agent and side-thread context visible without replacing the
custom status line.

- Active child agents are listed above the active task list in the bottom pane.
- Each active-agent row shows the task name, elapsed runtime, provider/model
  metadata when available, and token usage when available.
- The active-agent list caps visible rows and summarizes overflow.
- Agent metadata comes from child-agent activity and thread session state. It
  does not fall back to the parent session provider/model.
- The footer renders active thread context on the right side, including side
  thread names and active-agent labels.
- Active thread labels are dimmed right-side context, not status-line segments.
- Goal status and other right-side indicators remain visible alongside the
  active thread label when space allows.
- Active agent lifecycle state is updated on spawn, resume, interrupt, message,
  and completion paths.

Primary files:

- `codex-rs/protocol/src/protocol.rs`
- `codex-rs/app-server-protocol/src/protocol/event_mapping.rs`
- `codex-rs/tui/src/app/agent_navigation.rs`
- `codex-rs/tui/src/app/session_lifecycle.rs`
- `codex-rs/tui/src/app/thread_routing.rs`
- `codex-rs/tui/src/bottom_pane/active_agent_list.rs`
- `codex-rs/tui/src/bottom_pane/footer.rs`
- `codex-rs/tui/src/bottom_pane/mod.rs`
- `codex-rs/tui/src/multi_agents.rs`

### Hook Output Visibility

The fork makes hook transcript output configurable while keeping the default TUI
quiet.

- `show_hook_output` defaults to `false`.
- Successful hooks with no visible output stay hidden after completion.
- Long-running hooks reveal only after a short delay to avoid viewport flashes.
- Quiet successful hooks linger briefly if they became visible.
- Failures, blocked/stopped hooks, and hooks with persistent output remain in
  history.
- Setting `show_hook_output = true` renders completed hook output entries.

Primary files:

- `codex-rs/config/src/config_toml.rs`
- `codex-rs/core/src/config/mod.rs`
- `codex-rs/core/config.schema.json`
- `codex-rs/tui/src/chatwidget/hook_lifecycle.rs`
- `codex-rs/tui/src/history_cell/hook_cell.rs`

### Multi-Provider Agent Message Delivery

The fork carries fixes for mixed-provider multi-agent sessions.

- Non-OpenAI parent providers send `spawn_agent`, `send_message`, and
  `followup_task` traffic as plaintext `agent_message` envelopes.
- OpenAI parent providers keep using encrypted content for agent-message tool
  traffic.
- Mixed OpenAI-to-Claude delivery avoids passing unreadable encrypted payloads
  to non-OpenAI child agents when plaintext is required.
- Agent-message serialization includes task name, sender, and payload headers
  for plaintext envelopes.
- Tests cover OpenAI encrypted delivery and non-OpenAI plaintext delivery.

Primary files:

- `codex-rs/core/src/agent/control.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/message_tool.rs`
- `codex-rs/core/src/tools/handlers/multi_agents_v2/spawn.rs`
- `codex-rs/core/tests/suite/subagent_notifications.rs`

### Inter-Agent Trace Diagnostics

The fork adds opt-in API tracing for debugging inter-agent request and stream
payload shape without writing full sensitive payloads by default.

- Setting `CODEX_INTER_AGENT_TRACE` to a file path enables JSONL trace output.
- The trace records request, websocket request, and stream-event summaries.
- Trace entries include request method/path, top-level body keys, input length,
  agent-message summaries, encrypted-content summaries, item identifiers, and
  compact string previews.
- Long string values are summarized rather than fully copied.
- The trace path is append-only for the current process.

Primary files:

- `codex-rs/codex-api/src/inter_agent_trace.rs`
- `codex-rs/codex-api/src/endpoint/responses_websocket.rs`
- `codex-rs/codex-api/src/endpoint/session.rs`
- `codex-rs/codex-api/src/sse/responses.rs`

### Provider Stream and Error Handling Fixes

The fork includes provider-compatibility fixes that should be preserved across
upstream rebases.

- Responses-provider streams preserve streamed assistant text even when a
  provider closes without a final completed response event.
- Claude prompt-too-long errors are mapped to the standard context-window
  exhaustion path so the TUI and core recovery behavior treat them consistently.

Primary files:

- `codex-rs/core/src/session/turn.rs`
- `codex-rs/core/tests/suite/stream_no_completed.rs`
- `codex-rs/codex-api/src/api_bridge.rs`
- `codex-rs/codex-api/src/api_bridge_tests.rs`

### Queued Input Recall Cleanup

The fork fixes TUI prompt-history recall when input has already been queued or
sent as a pending steer.

- Recalling a queued message removes the matching queued input instead of
  leaving it to be submitted later.
- Recalling a pending steer removes the matching pending steer.
- Stale pending-steer operations are detected and ignored after the matching
  input has been recalled.
- Recall matching handles text, image attachments, and paste placeholders.

Primary files:

- `codex-rs/tui/src/app_command.rs`
- `codex-rs/tui/src/chatwidget/input_queue.rs`
- `codex-rs/tui/src/chatwidget/input_restore.rs`
- `codex-rs/tui/src/chatwidget/input_submission.rs`
- `codex-rs/tui/src/chatwidget/user_messages.rs`

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
  clean before packaging by default. Pass `--allow-dirty --skip-github-release`
  to build a local package from uncommitted manifest or build-number changes;
  dirty builds cannot create or upload a GitHub release.
- macOS package signing requires a non-placeholder Developer ID Application
  identity.
- The helper builds `codex-cli` with Cargo `--locked`, signs the binary, verifies
  the signature, and invokes the Codex package builder with an explicit
  entrypoint and version.
- Release builds preserve incremental Cargo artifacts by using the current
  checkout and `codex-rs/target` as the default target directory.
- The helper limits default Cargo parallelism while respecting explicit caller
  settings through `--cargo-build-jobs`, `APOHL79_CARGO_BUILD_JOBS`, or Cargo's
  native `CARGO_BUILD_JOBS`.
- The helper can auto-repair stale workspace package versions in
  `codex-rs/Cargo.lock` before a locked release build.
- `scripts/apohl79_build_number.txt` stores the monotonically increasing fork
  build number. Fork release versions use
  `[codex-version]-apohl79-[build-number]` and GitHub release tags use
  `rust-v[codex-version]-apohl79-[build-number]`.
- The installer targets GitHub releases in `apohl79/codex`, resolves the
  current fork tag from a checked-out tag or `[workspace.package].version` plus
  the tracked build number, and verifies the uploaded asset SHA-256 before
  installing.
- The release helper creates the matching GitHub release in `apohl79/codex` if
  it does not already exist.
- The release helper uploads generated archives to the release with clobbering
  enabled so rebuilding the same version replaces the previous asset.
- The release helper uses the stored `apohl79` `gh` token by default for release
  publishing, while respecting an explicit `GH_TOKEN` or `GITHUB_TOKEN`.

### macOS Binary Auto-Updates

The macOS apohl79 standalone binary checks `apohl79/codex` for the latest
release when an interactive TUI session starts. When a newer fork build is
available, Codex offers the existing update prompt. Confirming the update runs
the fork installer, which verifies the release archive SHA-256, switches the
standalone package symlink, and launches the new `codex` binary with the
original arguments.

- This behavior is limited to macOS apohl79 release builds.
- Other installation methods and all non-macOS targets retain the upstream
  update behavior.

Primary files:

- `codex-rs/tui/src/update_action.rs`
- `codex-rs/tui/src/update_prompt.rs`
- `codex-rs/tui/src/updates.rs`
- `codex-rs/tui/src/update_versions.rs`
- `codex-rs/cli/src/main.rs`

Primary files:

- `scripts/apohl79_release.py`
- `scripts/apohl79_build_number.txt`
- `scripts/build_apohl79_release.py`
- `scripts/build_apohl79_release.sh`
- `scripts/install/install-apohl79.sh`
- `scripts/test_apohl79_release.py`
- `scripts/codex_package/cli.py`

### Fork Upgrade Tooling

The fork includes a local Codex skill for upgrading this fork from upstream
OpenAI Codex release tags and a local report skill for checking upstream
release drift.

- The skill documents the fork upgrade workflow.
- The skill includes an `openai` subagent for upstream inspection.
- The workflow preserves fork-only changes while rebasing or replaying
  `main-fork` onto a requested upstream release.
- The upstream-changes skill lists stable upstream `rust-vX.Y.Z` releases
  between the current apohl79 fork base and the latest non-alpha OpenAI Codex
  tag.
- The upstream-changes report excludes fork-added behavior from the main
  changelog and uses this file only to flag heuristic overlaps with fork
  features/fixes.

Primary files:

- `.codex/skills/upgrade-apohl79-fork/SKILL.md`
- `.codex/skills/upgrade-apohl79-fork/agents/openai.yaml`
- `.codex/skills/list-apohl79-fork-upstream-changes/SKILL.md`
- `.codex/skills/list-apohl79-fork-upstream-changes/scripts/list_apohl79_fork_upstream_changes.py`

### Repository Hygiene

The fork carries small repository-maintenance changes that support local
development and release hygiene.

- `.gitleaksignore` includes entries for upstream fixture secrets that can trip
  local scans.
- Generated files and TUI snapshots are refreshed after release rebases when
  upstream changes require it.
- The workspace version is pinned to the current fork release line.
- `scripts/apohl79_build_number.txt` is incremented for each feature/fix merged
  to `main-fork`.

Primary files:

- `.gitleaksignore`
- `codex-rs/Cargo.toml`
- `codex-rs/tui/src/**/*.snap`
- `scripts/apohl79_build_number.txt`

### Plugin Context

Plugins can declare static, position-aware instruction blocks in `plugin.json`
that Codex injects into every model API call via the `ContextContributor`
pipeline. This replaces hook-based periodic reminders with zero per-turn
overhead.

```json
{
  "context": {
    "thread": [
      {
        "slot": "contextual_user",
        "position": "preamble",
        "text": "Repository-wide rules: never commit secrets."
      },
      {
        "slot": "contextual_user",
        "position": "supplement",
        "text": "[post-turn] Check whether project context needs updating."
      }
    ]
  }
}
```

**Slots** route to API message roles:

| Slot | API role | Behavior |
|------|----------|----------|
| `developer_policy` | Developer (system) | Aggregated with other developer instructions |
| `developer_capabilities` | Developer (system) | Same bucket as `developer_policy` |
| `contextual_user` | User message | Same slot as AGENTS.md; diffed and persisted |
| `separate_developer` | Separate developer message | Isolated from other developer sections |

**Position** only applies to `contextual_user` slot entries. For
`developer_policy`, `developer_capabilities`, and `separate_developer`, the
position field is ignored — those slots map to system/developer messages where
there is no AGENTS.md ordering concept.

| Position | Slot | Effect |
|----------|------|--------|
| `preamble` | `contextual_user` only | Before AGENTS.md |
| `supplement` | `contextual_user` only | After AGENTS.md |

Plugin context is thread-scoped — subagents automatically inherit it.
Content is static (no templates) and read once at plugin load time.

## Notes For Maintainers

- Refresh this file after rebasing `main-fork` onto a newer upstream baseline.
- Feature and fix branches are short-lived staging or review branches. Do not
  maintain them as durable fork inventory once `main-fork` contains their
  commits.
- Use behavior-level summaries rather than raw commit counts.
- Keep the selected `@` picker row visually distinct. The fork's expected
  selected-row background is `Color::Rgb(55, 60, 67)`.
