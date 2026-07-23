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

Install the apohl79 fork release on macOS:

```shell
curl -fsSL https://raw.githubusercontent.com/apohl79/codex/main-fork/scripts/install/install-apohl79.sh | sh
```

Or clone and build from `main-fork` on macOS or Linux:

```shell
git clone --branch main-fork https://github.com/apohl79/codex.git && cd codex/codex-rs && cargo install --locked --path cli
```

Then simply run `codex` to get started.

### Fork features

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

### AGENTS.md Active Reminders

The fork uses phase-tagged checklist items in `AGENTS.md` to keep the model
aware of mandatory steps at key workflow boundaries. The format is:

```markdown
## Active reminders

- [phase] description of the required action
```

Supported phases:

| Phase | When the model checks it |
|-------|--------------------------|
| `pre-edit` | Before modifying any source file |
| `pre-commit` | Before running `git commit` |
| `post-turn` | After completing a task or subtask, before reporting to the user |

Phases are intentionally sparse — only add a phase when a documented
checklist item needs it. Multiple phases per item: `[pre-commit, pre-push]`.

The model reads this section at session start and re-checks it when reaching
each tagged phase. The section lives at the top of `AGENTS.md` so it cannot
be scrolled past during pre-commit workflows.

See [README.fork.md](./README.fork.md) for the full fork inventory.

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
