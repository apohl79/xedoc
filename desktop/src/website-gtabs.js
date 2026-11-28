"use strict";

const fs = require("node:fs");
const path = require("node:path");
const { BrowserWindow, dialog, session } = require("electron");
const state = require("./state");
const { validatedWebsiteURL } = require("./urls");

let extension;
let busy = false;
const windows = new Set();

async function load(directory, ensureRuntime) {
  const manifest = JSON.parse(fs.readFileSync(path.join(directory, "manifest.json"), "utf8"));
  if (manifest.manifest_version !== 3 || manifest.name !== "Gtabs Ultimate Guitar bridge") {
    throw new Error("Select the gTabs extension folder containing manifest.json.");
  }
  ensureRuntime();
  if (extension) session.defaultSession.extensions.removeExtension(extension.id);
  extension = null;
  extension = await session.defaultSession.extensions.loadExtension(directory);
  state.set("gtabsExtensionPath", directory);
}

async function initialize(ensureRuntime) {
  const directory = state.get("gtabsExtensionPath");
  if (typeof directory !== "string" || !directory) return;
  try {
    await load(directory, ensureRuntime);
  } catch (error) {
    console.error("Could not load gTabs:", error.message);
  }
}

async function configure(ensureRuntime, pickDirectory, changed) {
  if (busy) return;
  busy = true;
  changed();
  try {
    let directory = state.get("gtabsExtensionPath");
    if (pickDirectory) {
      const result = await dialog.showOpenDialog({
        title: "Load gTabs extension",
        properties: ["openDirectory"],
        defaultPath: directory,
      });
      if (result.canceled) return;
      directory = result.filePaths[0];
    }
    await load(directory, ensureRuntime);
    await dialog.showMessageBox({
      type: "info",
      title: "gTabs",
      message: "gTabs loaded. Reload open gTabs and Ultimate Guitar pages to activate it.",
    });
  } catch (error) {
    dialog.showErrorBox("gTabs", error.message);
  } finally {
    busy = false;
    changed();
  }
}

function canOpen(text) {
  if (!extension) return false;
  const url = validatedWebsiteURL(text);
  return url?.origin === "https://www.ultimate-guitar.com" &&
    url.pathname === "/user/playlist";
}

function createTab(details, runtime) {
  if (windows.size >= 16) throw new Error("Too many gTabs windows.");
  const window = new BrowserWindow({
    title: "Ultimate Guitar",
    width: 1100,
    height: 800,
    show: details.active !== false,
    webPreferences: {
      session: session.defaultSession,
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
      devTools: false,
    },
  });
  windows.add(window);
  window.once("closed", () => windows.delete(window));
  const contents = window.webContents;
  contents.on("will-navigate", (event) => {
    if (!validatedWebsiteURL(event.url)) event.preventDefault();
  });
  contents.on("will-redirect", (event) => {
    if (event.isMainFrame && !validatedWebsiteURL(event.url)) event.preventDefault();
  });
  contents.setWindowOpenHandler(() => ({ action: "deny" }));
  runtime.addTab(contents, window);
  contents.loadURL(details.url).catch(() => {});
  // A background sync tab must still be reachable for sign-in.
  window.showInactive();
  return [contents, window];
}

function menu(ensureRuntime, changed) {
  return [
    { label: "Load gTabs Extension…", enabled: !busy, click: () => void configure(ensureRuntime, true, changed) },
    ...(state.get("gtabsExtensionPath") ? [
      { label: extension ? `gTabs ${extension.version} Loaded` : "gTabs unavailable", enabled: false },
      { label: "Reload gTabs Extension", enabled: !busy, click: () => void configure(ensureRuntime, false, changed) },
      { label: "Remove gTabs Extension", enabled: !busy, click: () => {
        if (extension) session.defaultSession.extensions.removeExtension(extension.id);
        extension = null;
        state.set("gtabsExtensionPath", undefined);
        changed();
      } },
    ] : []),
  ];
}

module.exports = { initialize, canOpen, createTab, menu };
