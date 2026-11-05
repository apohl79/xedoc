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
  "mainJsonlPath": null
}
```

The process must print one JSON line within 180 seconds:
`{"url":"http://127.0.0.1:12345/","title":"Inline Discussion"}`.
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
