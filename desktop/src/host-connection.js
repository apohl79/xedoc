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
const { validatedHostURL, validatedWebsiteURL, sameOrigin } = require("./urls");

const background = "#12171c";
const configuredSessions = new WeakSet();

function isLocalHostURL(url) {
  return readLocalHost()?.url === url;
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

function configureSession(hostSession) {
  if (configuredSessions.has(hostSession)) return;
  configuredSessions.add(hostSession);
  const allowed = new Set(["clipboard-sanitized-write", "fullscreen"]);
  hostSession.setPermissionRequestHandler((contents, permission, callback, details) => {
    if (permission === "openExternal") {
      // Only same-origin host frames may hand Slack links to the system, like the macOS app.
      const target = details.externalURL ?? "";
      callback(/^slack:/i.test(target) && sameOrigin(details.requestingUrl, contents.getURL()));
      return;
    }
    callback(allowed.has(permission));
  });
  hostSession.setPermissionCheckHandler((_contents, permission) => allowed.has(permission));
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
    this.error = null;
    this.certificateError = false;
    this.username = null;
    this.pendingRoute = null;
    this.localAccessToken = null;
    this.retryLocalConnection = false;
    this.closed = false;

    const hostSession = session.fromPartition(this.partition);
    configureSession(hostSession);
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
    if (contents === this.contents) {
      if (template.length) template.push({ type: "separator" });
      template.push(...this.commandMenu());
    }
    while (template.at(-1)?.type === "separator") template.pop();
    if (template.length) Menu.buildFromTemplate(template).popup({ window: this.window });
  }

  // The window has no menu bar, so the commands of the macOS File menu live in the context menu too.
  commandMenu() {
    return [
      { label: "Hosts…", accelerator: "CommandOrControl+Shift+N", registerAccelerator: false, click: () => this.showHosts() },
      { label: "New Tab…", accelerator: "CommandOrControl+Shift+T", registerAccelerator: false, click: () => this.openSessionMenu() },
      { label: "Reload Host", accelerator: "CommandOrControl+Shift+R", registerAccelerator: false, click: () => this.load() },
      { label: "Pair Device…", click: () => this.pairDevice() },
      { label: "Open Website…", accelerator: "CommandOrControl+Shift+L", registerAccelerator: false, click: () => this.promptWebsite() },
      { type: "separator" },
      { label: "Quit Xedoc", accelerator: "CommandOrControl+Shift+Q", registerAccelerator: false, role: "quit" },
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
        const url = validatedWebsiteURL(event.url);
        if (this.websites.size >= 64 || panel || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)
            || !url) return;
        const created = new WebsitePanel({
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

  removeWebsite(id) {
    const panel = this.websites.get(id);
    this.websites.delete(id);
    if (!panel) return;
    if (!this.window.isDestroyed()) this.window.contentView.removeChildView(panel.view);
    panel.close();
  }

  closeWebsites() {
    for (const id of [...this.websites.keys()]) this.removeWebsite(id);
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
    this.prompt = this.dialog("open-website.html", 420, 170, {
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
      maximizable: false, show: false, backgroundColor: background, autoHideMenuBar: true,
      webPreferences: uiWebPreferences(),
    });
    window.removeMenu();
    registerUI(window.webContents, { ...handler, close: () => window.close() });
    window.once("ready-to-show", () => window.show());
    window.loadFile(uiPage(page));
    return window;
  }

  close() {
    this.closed = true;
    clearInterval(this.localMonitor);
    this.closeWebsites();
    if (this.overlay && !this.overlay.webContents.isDestroyed()) this.overlay.webContents.close();
  }
}

function windowTitle(url) {
  const parsed = new URL(url);
  return parsed.port ? `${parsed.hostname}:${parsed.port}` : parsed.hostname || "Xedoc";
}

module.exports = { HostConnection, isLocalHostURL, partitionFor };
