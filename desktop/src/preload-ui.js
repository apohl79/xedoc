"use strict";

// Preload for the app's own windows (hosts, dialogs, error overlay).
const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("xedoc", {
  platform: process.platform,
  invoke: (name, ...args) => ipcRenderer.invoke("xedoc:ui", name, ...args),
  on: (name, callback) => ipcRenderer.on("xedoc:ui-event", (_event, eventName, payload) => {
    if (eventName === name) callback(payload);
  }),
});
