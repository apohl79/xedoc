<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
This `apohl79/codex` fork tracks upstream Codex on `main-fork` with substantial agent improvements: persistent task lists, cross-model sub-agent routing, active-agent UI, multi-provider error handling, and TUI productivity enhancements.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Quickstart

### Installing and running this fork

Install the latest apohl79 fork release on macOS or Linux:

```shell
curl -fsSL https://raw.githubusercontent.com/apohl79/codex/main-fork/scripts/install/install-apohl79.sh | bash
```

Or clone and build from `main-fork` on macOS or Linux:

```shell
git clone --branch main-fork https://github.com/apohl79/codex.git && cd codex/codex-rs && cargo install --locked --path cli
```

Then simply run `codex` to get started.

### Fork features

- Plugin context declarations in `plugin.json` for permanent, position-aware
  model instructions injected via `ContextContributor` — zero-overhead replacement
  for hook-based periodic reminders (preamble/supplement slots relative to AGENTS.md,
  see [Plugin Context](#plugin-context) below).
- Extended TUI `@` file-path completion.
- Custom TUI status line support.
- Active agent and active thread context in the bottom pane.
- TUI session names shown at the top-right of the user entry box.
- Automatic short session names with a `/rename --auto on|off` toggle and
  `/rename` override.
- Persistent active task list above the user entry field while a turn is running.
- Optional hook output rendering with quieter default hook history.
- Multi-provider agent message handling and inter-agent trace diagnostics.
- Responses-provider and Claude context-window error handling fixes.
- Queued-input recall cleanup for TUI prompt history.
- apohl79 release packaging and installer scripts.
- Fork upgrade and upstream-change report skills.
- City Lights (Doom Emacs) color theme for the TUI.

- **Multi-model harness via auth2api** — use Codex with DeepSeek or Claude models
  through the [auth2api](https://github.com/apohl79/auth2api) OAuth-to-API proxy. The fork includes
  per-model prompt adjustments for non-OpenAI reasoning budgets, the
  `codex-manager` profile wizard for zero-config Claude and DeepSeek setup, and
  cross-provider sub-agent support (e.g., Claude parent agent spawning GPT or
  DeepSeek child agents in the same session).

### Plugin Context

Plugins can declare static, position-aware instruction blocks in `plugin.json`
that Codex injects into every model API call — permanently, with zero per-turn
overhead. This replaces hook-based periodic reminders that pollute context over
long sessions.

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

**Slots** route content to specific API message roles:

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
| `preamble` | `contextual_user` only | Inserted **before** AGENTS.md |
| `supplement` | `contextual_user` only | Inserted **after** AGENTS.md |

Plugin context is thread-scoped — subagents automatically inherit it.
Content is static (no templates) and read once at plugin load time.



### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Fork notes**](./README.fork.md)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
