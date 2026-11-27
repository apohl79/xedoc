"use strict";

const { contextBridge } = require("electron");

contextBridge.executeInMainWorld({
  func: () => {
    if (
      globalThis.location?.origin !==
      "chrome-extension://aeblfdkhhhdcdjpifhhbdiojplfjncoa"
    )
      return;

    // Electron-hosted extension pages otherwise remain uncontrolled clients.
    self.addEventListener("activate", (event) => {
      event.waitUntil(clients.claim());
    });
    void clients.claim().catch(() => {});
  },
});
