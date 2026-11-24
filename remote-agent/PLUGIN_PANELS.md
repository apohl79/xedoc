# Host GUI plugin panels

This optional Xedoc-only contribution lives in `.xedoc-plugin/plugin.json`.
Other harness manifests and skill commands remain independent.

```json
{
  "gui": {
    "panels": [{
      "id": "discussion",
      "title": "Inline Discussion",
      "entrypoint": "./runtime/bin/xedoc-panel",
      "interpreter": "python",
      "linkHandlers": [{
        "id": "discuss-markdown",
        "title": "Start discussion",
        "extensions": [".md", ".markdown"],
        "schemes": []
      }]
    }]
  }
}
```

Only enabled, installed plugins contribute actions. `plugin/read.localPath`
identifies the materialized package on the app-server host. Entrypoints must
resolve within that package. Optional `interpreter` is `python` (the host
Python) or `node` (from `PATH`); otherwise the entrypoint is executed directly.
Launching an action runs trusted plugin code as the host user, without a shell.
Hover and focus only discover actions; they never execute plugin code.

## Runtime preparation

Plugins may declare `gui.prepare` alongside `gui.panels`:

```json
{"entrypoint":"./runtime/bin/prepare-runtime.mjs","interpreter":"node"}
```

Xedoc runs this trusted command without a shell during explicit plugin installation
and when enabled installed plugins are refreshed after a configured marketplace
upgrade. It runs from the plugin root before replacing the installed cache, so a
failed preparation leaves the previous installed plugin intact. Disabled plugins
declaring preparation are not prepared or replaced during upgrades.
Discovery-only refresh defers cache replacement for plugins declaring preparation
until an authorized installation or enabled marketplace upgrade succeeds. Catalog
listing, plugin reads, hover, and focus never run preparation.

The entrypoint must resolve to a file inside the plugin. `interpreter` is `node`
(resolved from `PATH`) or omitted for a directly executable entrypoint. Preparation
receives no stdin, has a 180-second timeout, and must be idempotent. Stdout is
discarded to protect RPC transports; use stderr for diagnostics. Installation or
upgrade reports failures with a retry instruction. Relative symlinks that resolve
inside the plugin are rewritten as portable relative links to their resolved
destinations in the installed cache. Absolute symlinks and links resolving outside
the plugin are rejected. Launchers may retain their own readiness fallback for
older hosts or externally changed local checkouts.

## Composer buttons

A panel may declare up to 16 `composerButtons`. The host displays them to the
left of the composer router/model controls:

```json
{
  "composerButtons": [{
    "id": "documents",
    "title": "Discuss a document",
    "icon": "▤",
    "mode": "menu",
    "entrypoint": "./runtime/bin/xedoc-composer",
    "interpreter": "python"
  }]
}
```

Place this property inside the panel declaration. IDs are unique within a panel.
Titles contain 1–120 characters; optional icons contain 1–4 characters.
Entrypoint containment and interpreter rules match panel entrypoints.
The session's `/api/plugin/panels` snapshot exposes `composerButtons` with
`pluginId`, `panelId`, `buttonId`, `title`, `icon`, and `mode`, never executable
paths. Discovery, polling, hover, and focus do not run composer commands.

Explicit clicks call `POST /api/plugin/composer` with `workspaceId`,
`projectName`, `threadId`, `pluginId`, `panelId`, and `buttonId`. The host
rechecks enabled-plugin status and runs the declared command without a shell,
with the project as working directory and one JSON line on stdin:

```json
{"protocol":"xedoc.plugin-composer/v1","pluginId":"inline-discussion@e47","panelId":"discussion","buttonId":"documents","threadId":"the-open-main-thread","projectRoot":"/absolute/project"}
```

This input contains no inference snapshot, socket credentials, or session
capabilities. Like panel entrypoints, commands are trusted local plugin code.
They must exit successfully within 10 seconds and write exactly one JSON line
of at most 256 KiB to stdout. Use stderr for diagnostics, not stdout.
The host discards composer stderr; launch failures remain explicit errors.

`mode: "menu"` returns a tree:

```json
{"items":[{"title":"docs","expanded":true,"children":[{"title":"guide.md","target":"docs/guide.md"}]}],"emptyMessage":"No Markdown documents"}
```

Branches have `children` and optional boolean `expanded` (default false);
leaves have `target`. Menus are limited to 1000 total nodes and 16 levels.
Node titles and the optional `emptyMessage` are limited to 255 characters;
targets to 4096. Control characters are not accepted.
Selecting a leaf calls the same endpoint with its `target`, without rerunning
the command. The host repeats project-access, existence, extension/scheme,
and enabled-plugin checks through the existing panel launch path.

`mode: "action"` runs immediately on click and returns either `{}` for a
completed action or `{"target":"docs/guide.md"}` to open that panel.
Actions cannot receive browser-supplied targets. A launch response is
`{"panel":{...}}`, using the same public panel object as the session snapshot.
Composer commands do not hold the shared panel lock while running.

Before offering a link action, the GUI calls `POST /api/plugin/actions` with
`workspaceId`, `projectName`, `threadId`, and `target` for its open session.
The response is `{"actions":[...]}`, containing only declared handlers that
accept the target. The host resolves source-range references and checks file
existence, canonical paths, and project access without launching a plugin.
Missing files, directories, unsupported targets, and targets outside a restricted
user's project produce no actions; catalog or infrastructure failures remain errors.
`POST /api/plugin/open` repeats validation at launch because files or access
may change after preflight. Plugins still validate their own input as needed.

The host passes one bounded JSON line on stdin:

```json
{
  "protocol": "xedoc.plugin-panel/v1",
  "pluginId": "inline-discussion@e47",
  "panelId": "discussion",
  "threadId": "the-open-main-thread",
  "projectRoot": "/absolute/project",
  "target": "/absolute/project/document.md",
  "home": "/absolute/xedoc-home",
  "sessionSocket": "/absolute/controller.sock",
  "sessionDir": "/absolute/private-panel-state",
  "mainJsonlPath": null,
  "inference": {
    "threadId": "the-open-main-thread",
    "model": "gpt-6-astra",
    "modelProvider": "openai",
    "reasoningEffort": "medium",
    "models": [{
      "id": "gpt-6-astra",
      "model": "gpt-6-astra",
      "providerId": "openai",
      "upgrade": null,
      "upgradeInfo": null,
      "availabilityNux": null,
      "displayName": "GPT-6 Astra",
      "description": "Example cached model",
      "hidden": false,
      "availability": "available",
      "supportedReasoningEfforts": [{
        "reasoningEffort": "medium",
        "description": "Medium reasoning"
      }],
      "defaultReasoningEffort": "medium",
      "inputModalities": ["text", "image"],
      "supportsPersonality": false,
      "additionalSpeedTiers": [],
      "serviceTiers": [],
      "defaultServiceTier": null,
      "isDefault": true
    }]
  }
}
```

`inference` is the host's current resolved main-thread selection and cached model
catalog, not browser-supplied settings. `reasoningEffort` may be `null`; each model
retains its explicit `providerId`, `supportedReasoningEfforts`, and
`defaultReasoningEffort` as in the native `model/list` response. Plugins use this
snapshot directly instead of starting another app-server, calling `model/list`
or `config/read`, or reading the rollout to discover their initial selection.
Standalone plugin launches may retain their own discovery path.

The host reads a private capability-protected loopback snapshot from the existing
session server. This reads only the current thread state and in-memory catalog:
it does not refresh providers, reload configuration, or wait for model discovery.
Catalog freshness follows Xedoc's existing refresh worker. Both browser actions
and local skill launches receive the same snapshot. Main-thread changes are
validated independently of the selected child thread in the browser.

Snapshots are limited to 1000 models and 1 MiB of serialized JSON. A busy, missing,
or oversized cache, an active provider/model absent from that cache, or a stale
main-thread identity fails the launch explicitly. It never silently chooses a
different provider or falls back to discovery; retry after the session/catalog
is ready. An existing live panel still reuses its process without fetching another
snapshot.

The process must print one JSON line within 180 seconds:
`{"url":"http://127.0.0.1:12345/","title":"discussion.md"}`.
The optional `title` sets the panel tab title (1–255 characters after trimming).
If omitted, the host uses the installed plugin's name. The manifest panel title
is optional and does not override the launch response.
Keep stdout reserved for this response and use stderr for diagnostics.
The host stores stderr in the private panel directory's `runtime.log`.
The process must handle termination and close its agents and connections.

The host authenticates panel ownership and proxies HTTP methods and SSE under
an unguessable `/plugin-ui/<capability>/` prefix. Logout, session close, workspace
removal, and host shutdown revoke access and stop the process. Panels are limited
to 16 per host. Reopening the same owner/thread/plugin/panel/target reuses the live
process and stable private state directory.

Panel iframes have an opaque origin: no `allow-same-origin`, host cookies, parent
DOM, host API access, or trusted postMessage channel. The proxy adds restrictive
CSP and `Access-Control-Allow-Origin: null` for the capability-scoped resource.
Both sandbox policies allow `allow-top-navigation-to-custom-protocols` so native
Slack links can reach the browser's external-protocol handler without navigating
the panel away. The host page's `frame-src` also permits `slack:` alongside
`'self'`; other external frame schemes remain blocked. Browser user-activation,
confirmation, and installed handler requirements still apply; Xedoc does not
launch external applications. This does not allow popups, ordinary top-level
navigation, or same-origin access.
Clients must support mounting below the prefix: the frame URL includes
`xedocBase=/plugin-ui/<capability>/`; an HTML `<base href="/">` is replaced with
the mount prefix. Use the prefix for requests, assets, navigation, and SSE.
Standalone servers retain their original origin and framing policy.

File handlers receive canonical paths, with source line ranges and fragments
removed. Relative paths resolve against the session project. Workspace users
cannot open targets outside that project; administrators may explicitly select
external files. Scheme handlers receive the original URL. Declarations cannot
register active-content schemes such as `javascript` or `data`.

## Session runtime reuse

A panel may opt in to `"reuse": "session"` in its declaration. The host starts
the enabled panel's runtime when the user explicitly opens its session GUI,
without supplying a document or waiting for imports. Catalog listing, hover, and
focus never start a runtime. A failed speculative warmup does not prevent opening
the main session GUI.

The host sets `XEDOC_PLUGIN_PANEL_REUSE=session` and keeps stdin open. After
imports, the runtime prints:

```json
{"protocol":"xedoc.plugin-panel-runtime/v1","ready":true}
```

It then handles serialized JSON-line commands. Every command and response carries
that protocol. Opening an instance supplies the unchanged panel context described
above, including a fresh inference snapshot and its own private `sessionDir`:

```json
{"protocol":"xedoc.plugin-panel-runtime/v1","op":"open","instanceId":"opaque-panel-id","context":{"protocol":"xedoc.plugin-panel/v1"}}
```

The `context` example is abbreviated. Success returns `instanceId`, `url`, and an
optional `title`, using the same root-only loopback URL validation as ordinary
panels. An instance failure returns `instanceId` and an `error` string without
stopping sibling instances. Every instance owns its HTTP server, document watcher,
inference settings, discussions, and Apply/Pause/Finish lifecycle. Sharing imports
does not share document state or public capability URLs.

`{"protocol":"xedoc.plugin-panel-runtime/v1","op":"close","instanceId":"opaque-panel-id"}`
closes only that instance and returns the protocol, `instanceId`, and `ok: true`.
Close must be idempotent. A `status` command returns the protocol and an
`instances` array of live IDs, so the host can remove instances closed by
Pause or Finish. Closing the last panel leaves the runtime warm.

Responses are limited to 64 KiB and commands to 2 MiB. Open/readiness waits are
limited to 180 seconds; close and status commands to five seconds. Invalid
framing, a mismatched response, timeout, or process failure invalidates the whole
runtime and all its capabilities rather than risking a desynchronized channel.
Stdout is reserved for protocol responses; diagnostics go to the runtime's private
`host-gui/runtimes/<identity>/runtime.log`.

The pool is scoped to the authenticated owner, thread, canonical project, plugin,
panel, entrypoint, interpreter, and manifest revision. Different owners or threads
never share a runtime. Disabling/removing a declaration or changing its revision
retires the old process when the catalog is refreshed. Logout, session close or
rebind, workspace removal, and host shutdown stop idle and active runtimes.
The host limits the total runtime processes and visible panel instances to 16
each. Non-opted-in panels retain the original one-process-per-target protocol.

## Skill launch

A skill can request the same panel through the owner-only local socket
`$XEDOC_HOME/remote-agent/host-panels.sock`. Send one JSON line with `protocol`,
`threadId`, `pluginId`, `panelId`, and `target` as above. The response is one
bounded JSON line with `ok: true` and the panel title (not its capability URL), or `ok: false`
and `error`. `unavailable: true` means no active host GUI session exists;
the skill may retain its standalone fallback. The host browser discovers the
panel through its normal session-scoped polling. No TUI keystrokes or model
skill execution are used by link actions.

The local socket is restricted to its OS owner. On platforms without Unix
sockets, browser link actions still work, but local skill launch is unavailable.
