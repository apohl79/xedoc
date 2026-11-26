"use strict";

// Connects the app's own HTML windows (preload-ui.js) to main-process handlers per webContents.
const path = require("node:path");
const { ipcMain, screen } = require("electron");
const state = require("./state");

const handlers = new Map();

ipcMain.handle("xedoc:ui", async (event, name, ...args) => {
  const handler = handlers.get(event.sender.id);
  if (!handler || typeof handler[name] !== "function" || event.senderFrame !== event.sender.mainFrame) {
    throw new Error(`Unknown action ${name}`);
  }
  return handler[name](...args);
});

function registerUI(webContents, handler) {
  const id = webContents.id;
  handlers.set(id, handler);
  // A dropped link or file must not replace the app page while it keeps these handlers.
  webContents.on("will-navigate", (event) => event.preventDefault());
  webContents.setWindowOpenHandler(() => ({ action: "deny" }));
  webContents.once("destroyed", () => handlers.delete(id));
}

function uiWebPreferences() {
  return { preload: path.join(__dirname, "preload-ui.js"), partition: "desktop-ui", sandbox: true, contextIsolation: true };
}

function uiPage(name) {
  return path.join(__dirname, "ui", name);
}

// Counterpart of NSWindow.setFrameAutosaveName: restore and persist window bounds under a name.
function autosaveBounds(window, name) {
  const key = `bounds:${name}`;
  const saved = state.get(key);
  if (saved && ["x", "y", "width", "height"].every((field) => Number.isFinite(saved[field]))) {
    const area = screen.getDisplayMatching(saved).workArea;
    const visible = saved.x < area.x + area.width && saved.x + saved.width > area.x
      && saved.y < area.y + area.height && saved.y + saved.height > area.y;
    if (visible) window.setBounds(saved);
  }
  let timer;
  const save = () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (!window.isDestroyed() && !window.isMinimized() && !window.isFullScreen()) {
        state.set(key, window.getNormalBounds());
      }
    }, 300);
  };
  window.on("resize", save);
  window.on("move", save);
  window.on("close", () => {
    clearTimeout(timer);
    if (!window.isMinimized() && !window.isFullScreen()) state.set(key, window.getNormalBounds());
  });
}

module.exports = { registerUI, uiWebPreferences, uiPage, autosaveBounds };
