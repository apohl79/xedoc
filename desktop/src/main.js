"use strict";

// Port of XedocApp.swift (DesktopAppDelegate): host windows, the Hosts window, and remembered hosts.
const { app, BrowserWindow, ipcMain, Menu, powerMonitor, session } = require("electron");
const notifications = require("./notifications");
const pairing = require("./pairing");
const state = require("./state");
const { HostConnection, partitionFor } = require("./host-connection");
const { HostDiscovery } = require("./discovery");
const { ensureWindowsLocalHostService, readLocalHost } = require("./local-host");
const { registerUI, uiWebPreferences, uiPage, autosaveBounds } = require("./ui-bridge");
const { validatedHostURL, sameOrigin } = require("./urls");
const { installZoom, zoomMenu, sizeDialog } = require("./zoom");

const background = "#12171c";
const connections = new Map();
let selector = null;
let quitting = false;

if (!app.requestSingleInstanceLock()) {
  app.exit(0);
}

installZoom();

function showHosts() {
  if (selector) {
    if (selector.isMinimized()) selector.restore();
    selector.show();
    selector.focus();
    return;
  }
  const window = new BrowserWindow({
    width: 520, height: 480, minWidth: 500, minHeight: 380, show: false, title: "Hosts",
    backgroundColor: background, titleBarStyle: "hidden",
    titleBarOverlay: { color: background, symbolColor: "#a0b3c5", height: 30 },
    webPreferences: uiWebPreferences(),
  });
  window.removeMenu();
  autosaveBounds(window, "Hosts");
  const discovery = new HostDiscovery();
  discovery.on("change", (snapshot) => {
    if (!window.isDestroyed()) window.webContents.send("xedoc:ui-event", "discovery", snapshot);
  });
  const failure = (error) => ({ error: error instanceof Error ? error.message : String(error) });
  let pairingWindow = null;
  let pairingDismissDisabled = false;
  const handlers = {
    state: () => ({ localHost: readLocalHost(), discovery: discovery.snapshot() }),
    localHost: () => readLocalHost(),
    refresh: () => {
      discovery.scan();
      return readLocalHost();
    },
    select: (url) => {
      openHost(url);
      pairingDismissDisabled = false;
      pairingWindow?.close();
    },
    connect: (text) => {
      const url = validatedHostURL(String(text ?? "").trim());
      if (!url) return false;
      openHost(url.href);
      return true;
    },
    previewInvitation: (text) => {
      try {
        const { origin, fingerprint } = pairing.parseInvitation(String(text ?? ""));
        return { origin, fingerprint };
      } catch (error) {
        return failure(error);
      }
    },
    pair: async (text) => {
      pairingDismissDisabled = true;
      try {
        return { url: await pairing.pair(pairing.parseInvitation(String(text ?? "")), pairing.deviceName()) };
      } catch (error) {
        pairingDismissDisabled = false;
        return failure(error);
      }
    },
    openPairing: () => {
      if (process.platform !== "darwin") return;
      if (pairingWindow) { pairingWindow.focus(); return; }
      const [parentWidth, parentHeight] = window.getSize();
      const width = Math.min(520, parentWidth - 50);
      const height = Math.min(520, parentHeight - 40);
      const sheet = new BrowserWindow({
        parent: window, modal: true, width, height, useContentSize: true,
        resizable: false, minimizable: false, maximizable: false, fullscreenable: false,
        show: false, title: "Pair a host", backgroundColor: "#222222",
        webPreferences: uiWebPreferences(),
      });
      pairingWindow = sheet;
      pairingDismissDisabled = false;
      sizeDialog(sheet, width, height);
      sheet.on("close", (event) => {
        if (pairingDismissDisabled && !quitting) event.preventDefault();
      });
      sheet.removeMenu();
      registerUI(sheet.webContents, handlers);
      sheet.once("ready-to-show", () => sheet.show());
      sheet.once("closed", () => { pairingWindow = null; discovery.scan(); });
      sheet.loadFile(uiPage("hosts.html"), { query: { pairing: "true" } });
    },
    pairingClosed: () => {
      if (pairingWindow) pairingWindow.close();
      else discovery.scan();
    },
  };
  registerUI(window.webContents, handlers);
  window.on("closed", () => {
    discovery.stop();
    if (selector === window) selector = null;
  });
  window.once("ready-to-show", () => window.show());
  window.loadFile(uiPage("hosts.html"));
  selector = window;
}

function openHost(text, { route = null, restoring = false } = {}) {
  const parsed = validatedHostURL(text);
  if (!parsed) return;
  const url = parsed.href;
  const isLocalHost = readLocalHost()?.url === url;
  const key = isLocalHost ? "local" : url;
  const existing = connections.get(key);
  // A new pairing for this origin gets a new store; the old window must not keep the previous identity.
  if (existing && !isLocalHost && existing.partition !== partitionFor(url)) dispose(key, existing);
  const current = connections.get(key);
  if (current) {
    current.show();
    if (route) current.navigate(route);
    return;
  }
  const connection = new HostConnection(url, {
    isLocalHost,
    autosaveName: isLocalHost ? "LocalHost" : `Host:${key}`,
    showHosts,
    openHost: (target) => openHost(target),
    authenticationChanged: (authenticated) => {
      const hosts = state.stringArray("connectedHosts").filter((host) => host !== key);
      if (connection.isLocalHost) state.set("connectedLocalHost", authenticated);
      else if (authenticated) hosts.push(key);
      state.set("connectedHosts", hosts);
      if (authenticated) {
        connection.show();
        selector?.close();
      } else {
        showHosts();
        if (restoring) connection.window.close();
      }
    },
  });
  connection.window.on("closed", () => {
    if (connections.get(key) === connection) dispose(key, connection);
  });
  // Logging off closes windows without before-quit; keep the hosts remembered like a normal quit.
  connection.window.on("session-end", () => {
    quitting = true;
  });
  connections.set(key, connection);
  if (!restoring) connection.show();
  if (route) connection.navigate(route);
  connection.load();
}

function dispose(key, connection) {
  connections.delete(key);
  if (!quitting) {
    state.set("connectedHosts", state.stringArray("connectedHosts").filter((host) => host !== key));
    if (connection.isLocalHost) state.set("connectedLocalHost", false);
  }
  connection.close();
  if (!connection.window.isDestroyed()) connection.window.destroy();
}

function focusedConnection() {
  const window = BrowserWindow.getFocusedWindow();
  for (const connection of connections.values()) {
    if (connection.window === window || connection.dialogs().includes(window)) return connection;
  }
  return null;
}

function connectionFor(contents) {
  for (const connection of connections.values()) {
    if (connection.contents === contents) return connection;
  }
  return null;
}

// Ctrl+Shift keeps plain Ctrl keys for terminals; macOS uses the native application menu.
const shortcuts = {
  n: () => showHosts(),
  t: () => focusedConnection()?.openSessionMenu(),
  r: () => focusedConnection()?.load(),
  l: () => focusedConnection()?.promptWebsite(),
  q: () => app.quit(),
};

app.on("web-contents-created", (_event, contents) => {
  if (process.platform === "darwin") return;
  contents.on("before-input-event", (event, input) => {
    const modifier = process.platform === "darwin" ? input.meta && !input.control : input.control && !input.meta;
    const action = shortcuts[input.key.toLowerCase()];
    if (input.type !== "keyDown" || !modifier || !input.shift || input.alt || !action) return;
    event.preventDefault();
    if (!input.isAutoRepeat) action();
  });
});

ipcMain.on("xedoc:host-init", (event) => {
  const connection = connectionFor(event.sender);
  event.returnValue = connection && event.senderFrame === event.sender.mainFrame
    && sameOrigin(event.senderFrame.url, connection.url) ? connection.restoreViewState() : null;
});

ipcMain.on("xedoc:desktop", (event, body) => {
  const connection = connectionFor(event.sender);
  if (connection && event.senderFrame === event.sender.mainFrame && sameOrigin(event.senderFrame.url, connection.url)) {
    connection.receive(body);
  }
});

ipcMain.on("xedoc:website", (event, body) => {
  const connection = connectionFor(event.sender);
  if (connection && event.senderFrame && sameOrigin(event.senderFrame.url, connection.url)) {
    connection.receiveWebsite(body);
  }
});

app.on("login", (event, _contents, _details, authInfo, callback) => {
  if (pairing.answerProxyLogin(authInfo, callback)) event.preventDefault();
});

app.on("second-instance", () => showHosts());
app.on("activate", () => {
  if (!BrowserWindow.getAllWindows().some((window) => window.isVisible())) showHosts();
});
app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
app.on("before-quit", () => {
  quitting = true;
});

app.whenReady().then(() => {
  require("./macos-migration").migrateMacOSState(state);
  if (process.platform === "darwin") {
    Menu.setApplicationMenu(Menu.buildFromTemplate([
      { role: "appMenu" },
      {
        label: "File",
        submenu: [
          { label: "Hosts…", accelerator: "Cmd+N", click: shortcuts.n },
          { label: "New Tab…", accelerator: "Cmd+T", click: shortcuts.t },
          { label: "Reload Host", accelerator: "Cmd+R", click: shortcuts.r },
          { label: "Pair Device…", click: () => {
            const connection = focusedConnection();
            if (connection) connection.pairDevice();
            else showHosts();
          } },
          { label: "Open Website…", accelerator: "Cmd+L", click: shortcuts.l },
          { type: "separator" },
          { role: "close" },
        ],
      },
      { role: "editMenu" },
      { label: "View", submenu: zoomMenu() },
      { role: "windowMenu" },
    ]));
  }
  ensureWindowsLocalHostService();
  // Website panels and the app's own pages use the default session; websites get no device or
  // notification access and cannot launch external protocol handlers.
  const allowed = new Set(["clipboard-sanitized-write", "fullscreen"]);
  session.defaultSession.setPermissionRequestHandler((_contents, permission, callback) => {
    callback(allowed.has(permission));
  });
  session.defaultSession.setPermissionCheckHandler((_contents, permission) => allowed.has(permission));
  powerMonitor.on("shutdown", () => {
    quitting = true;
  });
  notifications.onOpenSession((route) => openHost(route.host, { route }));
  const remembered = state.stringArray("connectedHosts").map(validatedHostURL).filter(Boolean)
    .map((url) => url.href);
  const local = readLocalHost();
  if (state.get("connectedLocalHost") === true && local) remembered.push(local.url);
  if (!remembered.length) showHosts();
  for (const url of remembered) openHost(url, { restoring: true });
  setTimeout(() => {
    if (!BrowserWindow.getAllWindows().some((window) => window.isVisible())) showHosts();
  }, 8_000);
});

module.exports = { showHosts, openHost };
