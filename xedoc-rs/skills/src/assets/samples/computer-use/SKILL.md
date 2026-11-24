---
name: computer-use
description: Use before every computer-use interaction through the `@computer-use` MCP session, including desktop clicks, typing, keyboard shortcuts, screenshots, and window management. Do not use `cua_repl`. Keep effects local to the intended target on a shared desktop; avoid interrupting Xedoc, this agent, other agents, or the user's work.
---

# Computer use on a shared desktop

The desktop, focus, keyboard, and clipboard are shared. An app-bound MCP call
does not prove input stays in that app. Never assume the foreground window is
still the one you last observed.

## Target, act, verify

1. Use the `@computer-use` MCP session available in the current environment.
   Read each tool's schema before its first use. Prefer read-only inspection or
   a narrower app/resource API over desktop input.
2. Identify the exact app, window, and control for the authorized task from
   fresh state. Treat Xedoc, terminals, editors, and other agent sessions as
   protected unless that exact surface is the requested target. Use read-only
   inventory first. Only running apps can be controlled; launching one requires
   explicit authorization.
3. Prefer a targeted accessibility action or control click. Before typing,
   using shortcuts, or entering coordinates, raise the intended window and
   verify its foreground/focus state and intended control. Recheck after
   dialogs, navigation, window changes, or another actor's input.
4. Perform one bounded action, then inspect its local result.

## Browser use

- When an authorized task needs a browser, use the system default browser.
  Preserve unrelated tabs and windows. Inspect the resulting window and verify
  the loaded page before interacting.
- Apply the same focus and recent-user-input safeguards as other desktop
  actions. Do not redirect an unrelated tab just because it is already open.

## The `@computer-use` session

- Use only the `@computer-use` MCP session. Do not use `cua_repl` or another
  computer-use runtime.
- If the session is unavailable, or health checks show missing permissions,
  stop and report the blocker. Do not install, reconnect, repair permissions,
  or alter integrations unless explicitly asked.
- Follow the current MCP client instructions for inspecting and calling tools.
- Before desktop work, verify that required permissions and capture health are
  available. If permission is missing, have the user grant it to the responsible
  GUI app; do not toggle permissions yourself.

## Do not disrupt sessions

- Never send global `Esc`, `Ctrl-C`, `Ctrl-D`, quit/close shortcuts, or repeated
  keys as generic cleanup or recovery. Prefer the intended dialog's explicit
  Cancel, Back, or Close control.
- Send a cancellation key only when the exact intended surface is freshly
  verified and cancelling that specific operation is authorized.
- Do not click Stop, close a terminal/tab/session, kill processes, restart apps
  or MCP sessions, log out, lock the screen, or change shared settings to
  recover from a computer-use failure without explicit authority for that
  target.
- Do not switch desktops, cycle through unrelated windows, alter the clipboard,
  or move/minimize unrelated windows just to find the target. Inspect windows
  first; raise only the known target.

On ambiguity, timeout, or unexpected focus: stop input, inspect safely, and
report the blocker. Never claim this policy technically isolates input.
