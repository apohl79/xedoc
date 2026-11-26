"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");
const {
  app,
  BrowserWindow,
  dialog,
  session,
  webContents,
} = require("electron");
const { EventEmitter } = require("node:events");
const state = require("./state");
const { validatedWebsiteURL } = require("./urls");

const onepasswordId = "aeblfdkhhhdcdjpifhhbdiojplfjncoa";
const panels = new Map();
const windows = new Set();
const clients = new Set();
const events = new EventEmitter();
let runtime;
let protectedSession;
let loaded = false;
let loading = false;
let codePath;
let disposeNative;
let transition = Promise.resolve();
let changing = false;

function protects(browserSession) {
  return browserSession === protectedSession;
}

async function initialize() {
  session.defaultSession.webRequest.onBeforeRequest(
    { urls: ["<all_urls>"] },
    (details, callback) => {
      const contents = details.webContents;
      const website = panels.has(contents);
      const extensionWindow = [...windows].some(
        (window) => window.webContents === contents,
      );
      callback({
        cancel:
          details.resourceType === "mainFrame" &&
          ((website && !validatedWebsiteURL(details.url)) ||
            (extensionWindow && !allowedExtensionURL(details.url))),
      });
    },
  );
  if (state.get("onepasswordEnabled") !== true) return;
  try {
    await setEnabled(true);
  } catch {
    console.error(
      "1Password could not start safely. Disable it from the Extensions menu or browser agent tool to restore raw DevTools.",
    );
  }
}

async function load() {
  if (!runtime) {
    delete process.env.DEBUG;
    require("debug").disable();
    for (const [module, digest] of [
      [
        "electron-chrome-extensions",
        "90411be233a92821b2873b9062584553d8a9f994a057f6efa34d32e267ee28ac",
      ],
      [
        "electron-chrome-extensions/preload",
        "7d763b069204552a7145fc52d7c6576ae13785c1ff3d24673eab1542bad043c3",
      ],
    ]) {
      if (
        createHash("sha256")
          .update(fs.readFileSync(require.resolve(module)))
          .digest("hex") !== digest
      ) {
        throw new Error("Unsafe extension runtime.");
      }
    }
    const { ElectronChromeExtensions } = require("electron-chrome-extensions");
    runtime = new ElectronChromeExtensions({
      session: protectedSession,
      requestPermissions: async () => false,
      createTab: async (details) => {
        const window = await createWindow(details.url);
        return [window.webContents, window];
      },
      selectTab: (contents, window) => {
        const activate = panels.get(contents);
        if (activate) activate();
        else {
          window.show();
          window.focus();
        }
      },
      removeTab: (contents) => {
        if (panels.has(contents)) return; // Website lifetime belongs to the host's tab model.
        if (!contents.isDestroyed()) contents.close();
      },
    });
    runtime.on("browser-action-popup-created", (popup) => {
      const window = popup.browserWindow;
      windows.add(window);
      window.once("closed", () => windows.delete(window));
      protectExtensionContents(window.webContents);
    });
  }
  disposeNative = require("./onepassword-native").installNativeBridge(runtime);
  if (!codePath) {
    const source = app.isPackaged
      ? path.join(process.resourcesPath, "onepassword")
      : path.join(__dirname, "../build/onepassword");
    const directory = fs.mkdtempSync(
      path.join(app.getPath("userData"), "extension-code-"),
    );
    app.once("quit", () => {
      try {
        fs.rmSync(directory, { recursive: true, force: true });
      } catch {}
    });
    const target = path.join(directory, "onepassword");
    fs.cpSync(source, target, {
      recursive: true,
      errorOnExist: true,
      force: false,
    });
    codePath = target;
  }
  loading = true;
  try {
    const extension = await protectedSession.extensions.loadExtension(codePath);
    if (extension.id !== onepasswordId) {
      protectedSession.extensions.removeExtension(extension.id);
      throw new Error("Unexpected extension identity.");
    }
    loaded = true;
  } finally {
    loading = false;
  }
}

function status() {
  return {
    enabled: !!protectedSession,
    loaded,
    rawDevtoolsAvailable: !protectedSession,
    panelsReloaded: 0,
  };
}

function setEnabled(enabled) {
  if (typeof enabled !== "boolean")
    return Promise.reject(new Error("enabled must be a boolean."));
  const operation = transition.then(async () => {
    if (enabled === !!protectedSession && (!enabled || loaded)) return status();
    changing = true;
    const restorers = [];
    let panelsReloaded = 0;
    // No in-flight debugger command may return privileged extension results.
    protectedSession = session.defaultSession;
    loaded = false;
    try {
      for (const suspend of clients) {
        const snapshot = await suspend();
        restorers.push(snapshot.restore);
        panelsReloaded += snapshot.count;
      }
      for (const window of BrowserWindow.getAllWindows()) {
        if (window.webContents.session === protectedSession) window.destroy();
      }
      await Promise.all(
        webContents
          .getAllWebContents()
          .filter((contents) => contents.session === protectedSession)
          .map(
            (contents) =>
              new Promise((resolve) => {
                contents.once("destroyed", resolve);
                contents.close({ waitForBeforeUnload: false });
              }),
          ),
      );
      disposeNative?.();
      disposeNative = null;
      protectedSession.extensions.removeExtension(onepasswordId);
      loaded = false;
      if (enabled) await load();
      else protectedSession = null;
      state.set("onepasswordEnabled", enabled);
      return { ...status(), panelsReloaded };
    } catch {
      // Keep raw DevTools blocked if any old privileged context might have survived.
      throw new Error(
        "Could not switch 1Password safely. Raw DevTools remains blocked; retry disabling 1Password.",
      );
    } finally {
      changing = false;
      for (const restore of restorers) restore();
      events.emit("change");
    }
  });
  transition = operation.catch(() => {});
  return operation;
}

function registerClient(suspend) {
  clients.add(suspend);
  return () => clients.delete(suspend);
}

function allowedExtensionURL(text) {
  try {
    const url = new URL(text);
    return (
      !url.username &&
      !url.password &&
      !url.port &&
      ((url.protocol === "chrome-extension:" &&
        url.hostname === onepasswordId) ||
        (url.protocol === "https:" &&
          ["1password.com", "1password.eu", "1password.ca"].some(
            (domain) =>
              url.hostname === domain || url.hostname.endsWith(`.${domain}`),
          )))
    );
  } catch {
    return false;
  }
}

async function createWindow(url) {
  if (
    (!loaded && !loading) ||
    !protectedSession ||
    !allowedExtensionURL(url) ||
    windows.size >= 16
  )
    throw new Error("Unsupported extension window.");
  const window = new BrowserWindow({
    title: "1Password",
    width: 900,
    height: 700,
    show: false,
    autoHideMenuBar: true,
    webPreferences: {
      session: protectedSession,
      sandbox: true,
      contextIsolation: true,
      devTools: false,
    },
  });
  windows.add(window);
  window.on("closed", () => windows.delete(window));
  const contents = window.webContents;
  protectExtensionContents(contents);
  runtime.addTab(contents, window);
  try {
    await window.loadURL(url);
    window.show();
    return window;
  } catch (error) {
    window.destroy();
    throw error;
  }
}

function protectExtensionContents(contents) {
  contents.on("will-navigate", (event) => {
    if (!allowedExtensionURL(event.url)) event.preventDefault();
  });
  contents.on("will-redirect", (event) => {
    if (event.isMainFrame && !allowedExtensionURL(event.url))
      event.preventDefault();
  });
  contents.setWindowOpenHandler((details) => {
    void createWindow(details.url).catch(() => {});
    return { action: "deny" };
  });
}

function registerPanel(contents, window, activate) {
  panels.set(contents, activate);
  contents.once("destroyed", () => panels.delete(contents));
  if (!runtime || !loaded) return;
  runtime.addTab(contents, window);
  contents.on("focus", () => selectPanel(contents));
}

function selectPanel(contents) {
  if (runtime && loaded && panels.has(contents)) runtime.selectTab(contents);
}

async function configure() {
  try {
    await setEnabled(!protectedSession);
  } catch (error) {
    dialog.showErrorBox("1Password", error.message);
  }
}

async function openOnePassword(contents) {
  try {
    await transition;
    if (!loaded) await setEnabled(true);
    if (
      !contents ||
      !panels.has(contents) ||
      !validatedWebsiteURL(contents.getURL())
    ) {
      await createWindow(
        `chrome-extension://${onepasswordId}/app/app.html#/page/welcome`,
      );
      return;
    }
    selectPanel(contents);
    const { width } = contents.getOwnerBrowserWindow()?.getContentBounds() || {
      width: 800,
    };
    runtime.api.browserAction.activateClick({
      extensionId: onepasswordId,
      tabId: contents.id,
      anchorRect: { x: Math.max(0, width - 80), y: 60, width: 32, height: 32 },
    });
  } catch {
    dialog.showErrorBox(
      "1Password",
      "Could not open 1Password. Try reloading the website or restarting Xedoc.",
    );
  }
}

function menu(contents) {
  return [
    { label: "Open 1Password", click: () => void openOnePassword(contents) },
    ...(protectedSession && !loaded
      ? [{ label: "1Password unavailable — disable or retry", enabled: false }]
      : []),
    {
      label: "1Password Enabled (reloads website panels)",
      type: "checkbox",
      checked: !!protectedSession,
      click: () => void configure(),
    },
  ];
}

module.exports = {
  initialize,
  protects,
  registerPanel,
  selectPanel,
  openOnePassword,
  menu,
  status,
  setEnabled,
  registerClient,
  events,
  changing: () => changing,
  whenStable: () => transition,
};
