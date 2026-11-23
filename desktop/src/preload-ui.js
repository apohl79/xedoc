"use strict";

// Preload for the app's own windows (hosts, dialogs, error overlay).
const { contextBridge, ipcRenderer } = require("electron");

document.addEventListener("DOMContentLoaded", () => {
  document.documentElement.dataset.platform = process.platform;
});
document.addEventListener("keydown", (event) => {
  if (process.platform === "darwin" && event.key === "Escape"
    && document.body.classList.contains("dialog-page")) document.getElementById("close")?.click();
});

contextBridge.exposeInMainWorld("xedoc", {
  platform: process.platform,
  invoke: (name, ...args) => ipcRenderer.invoke("xedoc:ui", name, ...args),
  on: (name, callback) => ipcRenderer.on("xedoc:ui-event", (_event, eventName, payload) => {
    if (eventName === name) callback(payload);
  }),
});
