"use strict";

// Port of HostConnection.swift: one host window with its own persistent web storage.
const crypto = require("node:crypto");
const path = require("node:path");
const { BrowserWindow, Menu, WebContentsView, clipboard, session, shell } = require("electron");
const notifications = require("./notifications");
const pairing = require("./pairing");
const state = require("./state");
const { localAccess, readLocalHost } = require("./local-host");
const { registerUI, uiWebPreferences, uiPage, autosaveBounds } = require("./ui-bridge");
const { WebsitePanel, describeLoadError } = require("./website-panel");
const { browserInput } = require("./browser-input");
const { validatedHostURL, validatedWebsiteURL, sameOrigin } = require("./urls");
const { displayHostName } = require("./ui/host-display-name");
const extensions = require("./website-extensions");

const background = "#12171c";
const configuredSessions = new WeakMap();

function isLocalHostURL(url) {
  return readLocalHost()?.url === url;
}

function normalizedOrigin(value) {
  try {
    return new URL(value).origin;
  } catch {
    return null;
  }
}

function partitionFor(url) {
  const paired = pairing.partitionFor(url);
  if (paired) return paired;
  const key = `hostStore:${url}`;
  let identifier = state.get(key);
  if (typeof identifier !== "string" || !/^[0-9a-f-]{36}$/.test(identifier)) {
    identifier = crypto.randomUUID();
    state.set(key, identifier);
  }
  return `persist:host-${identifier}`;
}

function configureSession(hostSession, contents, hostOrigin) {
  let trustedContents = configuredSessions.get(hostSession);
  if (trustedContents) {
    trustedContents.set(contents, hostOrigin);
    return;
  }
  trustedContents = new WeakMap([[contents, hostOrigin]]);
  configuredSessions.set(hostSession, trustedContents);
  const allowed = new Set(["clipboard-sanitized-write", "fullscreen"]);
  hostSession.setPermissionRequestHandler((contents, permission, callback, details) => {
    if (permission === "media") {
      const mediaTypes = details?.mediaTypes;
      callback(trustedContents.get(contents) === normalizedOrigin(details?.securityOrigin)
        && mediaTypes?.length === 1
        && mediaTypes[0] === "audio");
      return;
    }
    if (permission === "openExternal") {
      // Only same-origin host frames may hand Slack links to the system, like the macOS app.
      const target = details.externalURL ?? "";
      callback(/^slack:/i.test(target) && sameOrigin(details.requestingUrl, contents.getURL()));
      return;
    }
    callback(allowed.has(permission));
  });
  hostSession.setPermissionCheckHandler((contents, permission, requestingOrigin, details) => {
    if (permission === "media") {
      return Boolean(contents && trustedContents.get(contents) === normalizedOrigin(requestingOrigin)
        && details?.mediaType === "audio");
    }
    return allowed.has(permission);
  });
}

class HostConnection {
  constructor(url, { isLocalHost, autosaveName, showHosts, openHost, authenticationChanged }) {
    this.url = url;
    this.isLocalHost = isLocalHost;
    this.showHosts = showHosts;
    this.openHost = openHost;
    this.authenticationChanged = authenticationChanged;
    this.viewStateKey = `hostView:${isLocalHost ? "local" : url}`;
    this.partition = partitionFor(url);
    this.websites = new Map();
    this.pendingWebsiteOpens = new Map();
    this.pendingWebsiteRestores = new Map();
    this.error = null;
    this.certificateError = false;
    this.username = null;
    this.pendingRoute = null;
    this.localAccessToken = null;
    this.retryLocalConnection = false;
    this.closed = false;

    const hostSession = session.fromPartition(this.partition);
    // The host page must be the window's own contents: only those receive the titlebar-area geometry
    // of the window controls overlay. Website panels and the error overlay are child views above it.
    this.window = new BrowserWindow({
      width: 1200, height: 800, minWidth: 720, minHeight: 480, show: false,
      title: windowTitle(url), backgroundColor: background,
      titleBarStyle: "hidden",
      titleBarOverlay: { color: background, symbolColor: "#a0b3c5", height: 30 },
      webPreferences: {
        session: hostSession,
        preload: path.join(__dirname, "preload-host.js"),
        sandbox: true,
        contextIsolation: true,
        nodeIntegrationInSubFrames: true,
      },
    });
    this.window.removeMenu();
    this.window.on("page-title-updated", (event) => event.preventDefault());
    this.contents = this.window.webContents;
    autosaveBounds(this.window, autosaveName);
    this.window.on("resize", () => this.layout());
    this.observeContents(this.contents);
    configureSession(hostSession, this.contents, new URL(url).origin);
    this.unregisterExtensions = extensions.registerClient(() => this.suspendWebsites());

    if (isLocalHost) {
      this.localMonitor = setInterval(() => {
        const local = localAccess();
        if (local && (local.url !== this.url || local.token !== this.localAccessToken || this.retryLocalConnection)) {
          this.load();
        }
      }, 2_000);
    }
  }

  layout() {
    const { width, height } = this.window.getContentBounds();
    this.overlay?.setBounds({ x: 0, y: 0, width, height });
  }

  observeContents(contents) {
    contents.on("did-fail-load", (_event, code, description, _url, isMainFrame) => {
      if (isMainFrame) this.showLoadError(code, description);
    });
    contents.on("render-process-gone", (_event, details) => {
      if (details.reason !== "clean-exit") this.setError("The host page stopped unexpectedly. Choose Retry to reload it.");
    });
    contents.on("will-frame-navigate", (details) => this.decideNavigation(details));
    contents.setWindowOpenHandler(({ url, disposition }) => {
      if (validatedWebsiteURL(url)) {
        this.openWebsite(url, sameOrigin(url, this.url) || disposition === "background-tab");
      }
      return { action: "deny" };
    });
    contents.on("context-menu", (_event, params) => this.showContextMenu(contents, params));
  }

  decideNavigation(details) {
    const target = details.url;
    if (sameOrigin(target, this.url) || target === "about:blank" || target.startsWith("about:srcdoc")) return;
    // Slack links continue to the openExternal permission check, which allows same-origin host frames only.
    if (/^slack:/i.test(target)) return;
    details.preventDefault();
    if (details.isMainFrame && validatedWebsiteURL(target)) this.openWebsite(target, false);
  }

  showContextMenu(contents, params) {
    const template = [];
    const link = validatedWebsiteURL(params.linkURL);
    if (link) {
      template.push(
        { label: "Open Link in External Browser", click: () => shell.openExternal(link.href) },
        { label: "Copy Link", click: () => clipboard.writeText(link.href) },
        { type: "separator" },
      );
    }
    const flags = params.editFlags;
    if (params.isEditable) {
      template.push(
        { role: "undo", enabled: flags.canUndo }, { role: "redo", enabled: flags.canRedo }, { type: "separator" },
        { role: "cut", enabled: flags.canCut },
        { role: "copy", enabled: flags.canCopy },
        { role: "paste", enabled: flags.canPaste },
        { role: "selectAll", enabled: flags.canSelectAll },
      );
    } else if (params.selectionText) {
      template.push({ role: "copy" });
    }
    if ([...this.websites.values()].some((panel) => panel.view.webContents === contents)) {
      if (template.length) template.push({ type: "separator" });
      template.push(...extensions.menu(contents));
    }
    if (contents === this.contents) {
      if (template.length) template.push({ type: "separator" });
      template.push(...this.commandMenu());
    }
    while (template.at(-1)?.type === "separator") template.pop();
    if (template.length) Menu.buildFromTemplate(template).popup({ window: this.window });
  }

  // The window has no menu bar, so the commands of the macOS File menu live in the context menu too.
  commandMenu() {
    const modifier = process.platform === "darwin" ? "Command+" : "Control+Shift+";
    return [
      { label: "Hosts…", accelerator: `${modifier}N`, registerAccelerator: false, click: () => this.showHosts() },
      { label: "New Tab…", accelerator: `${modifier}T`, registerAccelerator: false, click: () => this.openSessionMenu() },
      { label: "Reload Host", accelerator: `${modifier}R`, registerAccelerator: false, click: () => this.load() },
      { label: "Pair Device…", click: () => this.pairDevice() },
      { label: "Open Website…", accelerator: `${modifier}L`, registerAccelerator: false, click: () => this.promptWebsite() },
      { label: "Extensions", submenu: extensions.menu() },
      { type: "separator" },
      { label: "Quit Xedoc", accelerator: `${modifier}Q`, registerAccelerator: false, role: "quit" },
    ];
  }

  dialogs() {
    return [this.pairing, this.prompt].filter((window) => window && !window.isDestroyed());
  }

  show() {
    if (this.window.isMinimized()) this.window.restore();
    this.window.show();
    this.window.focus();
  }

  async load() {
    if (this.closed) return;
    this.closeWebsites();
    this.setError(null);
    this.retryLocalConnection = false;
    this.identityFailed = false;
    const local = localAccess();
    if (this.isLocalHost && local) {
      this.url = local.url;
      this.localAccessToken = local.token;
      const { host } = new URL(this.url);
      this.window.setTitle(new URL(this.url).port ? host : `${host}:80`);
    }
    configureSession(session.fromPartition(this.partition), this.contents, new URL(this.url).origin);
    if (!this.isLocalHost) {
      try {
        await pairing.configureSession(session.fromPartition(this.partition), this.url, {
          onIdentityFailure: () => {
            this.identityFailed = true;
            this.certificateError = false;
            this.setError("Host identity verification failed. Obtain a new pairing invitation directly from your trusted host.");
          },
        });
      } catch (error) {
        this.setError(error.message);
        return;
      }
    }
    if (this.closed) return;
    const options = local && sameOrigin(local.url, this.url)
      ? { extraHeaders: `Authorization: Bearer ${local.token}\n` } : {};
    this.contents.loadURL(this.url, options).catch(() => {});
  }

  restoreViewState() {
    const saved = state.get(this.viewStateKey);
    return saved && typeof saved === "object" ? JSON.stringify(saved) : null;
  }

  receive(event) {
    if (!event || typeof event !== "object") return;
    switch (event.kind) {
      case "viewState": {
        const view = event.view;
        const name = view?.username;
        const keys = ["workspaceId", "projectName", "threadId"];
        if (typeof name !== "string" || !name || name.length > 64 || !keys.every((key) => view[key] === null
            || (typeof view[key] === "string" && Buffer.byteLength(view[key]) <= 1024))) return;
        state.set(this.viewStateKey, Object.fromEntries(["username", ...keys].map((key) => [key, view[key]])));
        break;
      }
      case "authenticated": {
        const name = event.username;
        if (typeof name !== "string" || !name || name.length > 64) return;
        const changed = this.username !== name;
        this.username = name;
        if (changed) this.authenticationChanged(true);
        if (this.pendingRoute) this.navigate(this.pendingRoute);
        break;
      }
      case "signedOut": {
        const local = localAccess();
        if (local && sameOrigin(local.url, this.url)) {
          this.load();
          return;
        }
        const wasAuthenticated = this.username !== null;
        this.username = null;
        if (wasAuthenticated || !this.window.isVisible()) this.authenticationChanged(false);
        break;
      }
      case "hosts":
        this.showHosts();
        break;
      case "pairDevice":
        this.pairDevice();
        break;
      case "openHost": {
        const target = validatedHostURL(event.url);
        if (target) this.openHost(target.href);
        break;
      }
      case "titlebar":
        break;
      case "website":
        this.receiveWebsiteCommand(event);
        break;
      case "browserAutomation":
        this.receiveBrowserAutomation(event);
        break;
      default:
        notifications.receive(event, this.url, this.username);
    }
  }

  receiveWebsite(body) {
    const url = validatedWebsiteURL(body?.url);
    if (url) this.openWebsite(url.href, body.external === true);
  }

  navigate(route) {
    this.pendingRoute = route;
    if (!this.username) return;
    this.pendingRoute = null;
    if (this.username !== route.username) {
      this.setError("This notification belongs to a different signed-in account.");
      return;
    }
    this.contents.executeJavaScript(`window.xedocOpenSession(${JSON.stringify(route)})`, true)
      .catch((error) => this.setError(`Could not open the notified session: ${error.message}`));
  }

  showLoadError(code, description) {
    // A failed host identity check already explains why the tunnel failed.
    if (code === -3 || this.identityFailed) return;
    this.retryLocalConnection = this.isLocalHost;
    this.certificateError = code <= -200 && code > -300;
    this.setError(this.certificateError
      ? "This host’s certificate is not trusted. Verify its CA fingerprint with the administrator, then install and trust the CA in your system certificate store."
      : describeLoadError(code, description));
    if (!this.window.isVisible()) this.showHosts();
  }

  setError(message) {
    this.error = message;
    if (!message) {
      this.overlay?.setVisible(false);
      return;
    }
    if (!this.overlay) {
      this.overlay = new WebContentsView({ webPreferences: uiWebPreferences() });
      this.overlay.setBackgroundColor(background);
      registerUI(this.overlay.webContents, {
        state: () => ({ error: this.error, certificateError: this.certificateError }),
        hosts: () => this.showHosts(),
        retry: () => this.load(),
        dismiss: () => this.setError(null),
        certificateSetup: () => shell.openExternal(new URL("trust", this.url).href),
      });
      this.overlay.webContents.loadFile(uiPage("error.html"));
    }
    this.window.contentView.addChildView(this.overlay);
    this.layout();
    this.overlay.setVisible(true);
    this.overlay.webContents.send("xedoc:ui-event", "state", { error: message, certificateError: this.certificateError });
  }

  openWebsite(url, external = false) {
    if (!validatedWebsiteURL(url)) return;
    if (external) {
      shell.openExternal(url);
      return;
    }
    this.contents.executeJavaScript(`window.xedocOpenWebsite?.(${JSON.stringify(url)})`, true).catch(() => {});
  }

  openSessionMenu() {
    this.contents.focus();
    this.contents.executeJavaScript("window.xedocOpenSessionMenu?.()", true).catch(() => {});
  }

  receiveWebsiteCommand(event) {
    const id = typeof event.id === "string" ? event.id : "";
    const panel = this.websites.get(id);
    switch (event.command) {
      case "open": {
        if (extensions.changing()) {
          this.pendingWebsiteOpens.set(id, event);
          void extensions.whenStable().then(() => {
            if (this.pendingWebsiteOpens.get(id) !== event) return;
            this.pendingWebsiteOpens.delete(id);
            if (!this.closed) this.receiveWebsiteCommand(event);
          });
          return;
        }
        const url = validatedWebsiteURL(event.url);
        if (this.websites.size >= 64 || panel || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)
            || !url) return;
        const created = new WebsitePanel({
          window: this.window,
          activate: () => {
            this.show();
            this.contents.executeJavaScript(`window.xedocActivateWebsite?.(${JSON.stringify(id)})`, true).catch(() => {});
          },
          changed: () => {
            if (this.websites.get(id) !== created) return;
            this.contents.executeJavaScript(`window.xedocWebsiteState?.(${JSON.stringify(created.state(id))})`, true)
              .catch(() => {});
          },
          openNew: (target) => {
            if (created.view.getVisible()) this.openWebsite(target);
          },
        });
        created.view.webContents.on("context-menu", (_event, params) => this.showContextMenu(created.view.webContents, params));
        this.websites.set(id, created);
        this.window.contentView.addChildView(created.view, 0);
        created.load(url.href);
        break;
      }
      case "layout": {
        for (const [key, item] of this.websites) item.view.setVisible(key === id);
        if (panel) extensions.selectPanel(panel.view.webContents);
        const rect = event.rect;
        const values = rect && [rect.x, rect.y, rect.width, rect.height];
        if (!panel || !values || !values.every(Number.isFinite) || rect.x < 0 || rect.y < 0
            || rect.width <= 0 || rect.height <= 0) return;
        const zoom = this.contents.getZoomFactor();
        panel.view.setBounds({
          x: Math.round(rect.x * zoom), y: Math.round(rect.y * zoom),
          width: Math.round(rect.width * zoom), height: Math.round(rect.height * zoom),
        });
        break;
      }
      case "close":
        this.pendingWebsiteOpens.delete(id);
        this.pendingWebsiteRestores.delete(id);
        if (panel) this.removeWebsite(id);
        break;
      case "clear":
        this.closeWebsites();
        break;
      case "back":
        panel?.view.webContents.navigationHistory.goBack();
        break;
      case "forward":
        panel?.view.webContents.navigationHistory.goForward();
        break;
      case "reload":
        panel?.view.webContents.reload();
        break;
      case "onepassword":
        if (panel) void extensions.openOnePassword(panel.view.webContents);
        break;
      case "external": {
        const url = panel && validatedWebsiteURL(panel.address);
        if (url) shell.openExternal(url.href);
        break;
      }
      case "navigate":
        if (!panel || typeof event.url !== "string") return;
        panel.submitAddress(event.url);
        panel.changed();
        break;
      default:
        break;
    }
  }

  receiveBrowserAutomation(event) {
    const requestId = event.requestId;
    if (typeof requestId !== "string"
        || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(requestId)) return;
    const reply = (result, error) => {
      if (this.contents.isDestroyed()) return;
      const response = { requestId, result: result ?? null };
      if (error !== undefined) response.error = String(error?.message || error || "Browser automation failed.").slice(0, 2048);
      this.contents.executeJavaScript(`window.xedocBrowserResult?.(${JSON.stringify(response)})`).catch(() => {});
    };
    const id = typeof event.panelId === "string" ? event.panelId : "";
    if (event.action === "extensions") {
      void (Object.hasOwn(event.input || {}, "enabled")
        ? extensions.setEnabled(event.input.enabled) : extensions.whenStable().then(() => extensions.status()))
        .then((result) => reply(result)).catch((error) => reply(null, error));
      return;
    }
    const panel = this.websites.get(id);
    if (!panel || panel.view.webContents.isDestroyed()) {
      reply(null, "Website panel is closed or missing.");
      return;
    }
    const contents = panel.view.webContents;
    switch (event.action) {
      case "cdp":
      case "cdp_events":
        void (event.action === "cdp" ? panel.devtools.command(event.input || {}) : panel.devtools.read(event.input || {}))
          .then((result) => reply(result)).catch((error) => reply(null, error));
        break;
      case "pointer":
      case "key":
      case "type":
        void browserInput(panel, event.action, event.input || {})
          .then((result) => reply(result)).catch((error) => reply(null, error));
        break;
      case "navigate":
        if (typeof event.url !== "string") reply(null, "Website URL is required.");
        else {
          panel.submitAddress(event.url);
          panel.changed();
          reply({ ok: true });
        }
        break;
      case "back":
        if (contents.navigationHistory.canGoBack()) contents.navigationHistory.goBack();
        reply({ ok: true });
        break;
      case "forward":
        if (contents.navigationHistory.canGoForward()) contents.navigationHistory.goForward();
        reply({ ok: true });
        break;
      case "reload":
        contents.reload();
        reply({ ok: true });
        break;
      case "close":
        this.removeWebsite(id);
        reply({ ok: true });
        break;
      case "screenshot":
        void contents.capturePage(undefined, { stayHidden: true, stayAwake: true }).then((image) => {
          const png = image.toPNG();
          if (!png.length) throw new Error("Could not capture website screenshot.");
          if (png.length > 4 * 1024 * 1024) throw new Error("Website screenshot exceeds 4 MiB.");
          if (this.websites.get(id) !== panel || contents.isDestroyed()) {
            throw new Error("Website panel was closed.");
          }
          reply({ base64: png.toString("base64"), mimeType: "image/png" });
        }).catch((error) => reply(null, error));
        break;
      default: {
        if (typeof event.script !== "string" || Buffer.byteLength(event.script) > 64 * 1024) {
          reply(null, "Browser script is missing or exceeds 64 KiB.");
          break;
        }
        void contents.executeJavaScript(`(async () => {
            const value = await (${event.script});
            const json = JSON.stringify(value === undefined ? null : value);
            if (typeof json !== "string") throw new Error("Browser result is not JSON-serializable.");
            if (new TextEncoder().encode(json).length > 49152) throw new Error("Browser result exceeds 48 KiB.");
            return json;
          })()`).then((json) => {
          if (this.websites.get(id) !== panel || contents.isDestroyed()) {
            throw new Error("Website panel was closed.");
          }
          if (typeof json !== "string" || Buffer.byteLength(json) > 48 * 1024) {
            throw new Error("Browser result is invalid or exceeds 48 KiB.");
          }
          reply(JSON.parse(json));
        }).catch((error) => reply(null, error));
      }
    }
  }

  removeWebsite(id) {
    const panel = this.websites.get(id);
    this.websites.delete(id);
    if (!panel) return;
    if (!this.window.isDestroyed()) this.window.contentView.removeChildView(panel.view);
    panel.close();
  }

  closeWebsites() {
    this.pendingWebsiteOpens.clear();
    this.pendingWebsiteRestores.clear();
    for (const id of [...this.websites.keys()]) this.removeWebsite(id);
  }

  async suspendWebsites() {
    const snapshots = [...this.websites].map(([id, panel]) => ({
      id, url: validatedWebsiteURL(panel.view.webContents.getURL())?.href || panel.address,
      bounds: panel.view.getBounds(), visible: panel.view.getVisible(),
    }));
    const closed = [...this.websites.values()].map((panel) =>
      new Promise((resolve) => panel.view.webContents.once("destroyed", resolve)));
    this.closeWebsites();
    for (const snapshot of snapshots) this.pendingWebsiteRestores.set(snapshot.id, snapshot);
    await Promise.all(closed);
    return {
      count: snapshots.length,
      restore: () => {
        if (this.closed) return;
        for (const snapshot of this.pendingWebsiteRestores.values()) {
          this.receiveWebsiteCommand({ command: "open", id: snapshot.id, url: snapshot.url });
          const panel = this.websites.get(snapshot.id);
          panel?.view.setBounds(snapshot.bounds);
          panel?.view.setVisible(snapshot.visible);
          if (snapshot.visible && panel) extensions.selectPanel(panel.view.webContents);
        }
        this.pendingWebsiteRestores.clear();
      },
    };
  }

  pairDevice() {
    if (this.pairing && !this.pairing.isDestroyed()) {
      this.pairing.focus();
      return;
    }
    this.show();
    this.pairing = this.dialog("pair-device.html", 560, 580, require("./pair-device").handlers(this));
  }

  promptWebsite() {
    if (this.prompt && !this.prompt.isDestroyed()) {
      this.prompt.focus();
      return;
    }
    const [width, height] = process.platform === "darwin" ? [392, 136] : [420, 170];
    this.prompt = this.dialog("open-website.html", width, height, {
      open: (text) => {
        const value = String(text ?? "").trim();
        const url = validatedWebsiteURL(value.includes("://") ? value : `https://${value}`);
        if (!url) return false;
        this.prompt?.close();
        this.openWebsite(url.href);
        return true;
      },
      cancel: () => this.prompt?.close(),
    });
  }

  dialog(page, width, height, handler) {
    const window = new BrowserWindow({
      parent: this.window, modal: true, width, height, resizable: false, minimizable: false,
      useContentSize: process.platform === "darwin",
      maximizable: false, show: false, backgroundColor: background, autoHideMenuBar: true,
      webPreferences: uiWebPreferences(),
    });
    require("./zoom").sizeDialog(window, width, height);
    window.removeMenu();
    registerUI(window.webContents, { ...handler, close: () => window.close() });
    window.once("ready-to-show", () => window.show());
    window.loadFile(uiPage(page));
    return window;
  }

  close() {
    this.closed = true;
    this.unregisterExtensions();
    clearInterval(this.localMonitor);
    this.closeWebsites();
    if (this.overlay && !this.overlay.webContents.isDestroyed()) this.overlay.webContents.close();
  }
}

function windowTitle(url) {
  const parsed = new URL(url);
  const hostname = displayHostName(parsed.hostname);
  return parsed.port ? `${hostname}:${parsed.port}` : hostname || "Xedoc";
}

module.exports = { HostConnection, isLocalHostURL, partitionFor };
