"use strict";

// Host window preload. Sandboxed preloads cannot require local files, so the page scripts live here.
const { contextBridge, ipcRenderer } = require("electron");

// Main-world host page integration, adapted from macos/Xedoc/Native.js. It runs before host scripts.
// The window controls overlay sits at the top right on Linux and Windows, so the tab strip reserves
// that area through the standard titlebar-area env() variables and serves as the drag region.
function hostNativeScript(isMac) {
  const send = (body) => window.webkit.messageHandlers.xedocDesktop.postMessage(body);
  window.addEventListener("xedoc-host-view", (event) => send({
    kind: "viewState", view: event.detail,
  }));
  const originalFetch = window.fetch;
  window.fetch = async (...args) => {
    const response = await originalFetch(...args);
    if (response.status === 401 && new URL(response.url).origin === location.origin) {
      send({ kind: "signedOut" });
    }
    return response;
  };
  window.addEventListener("xedoc-host-ready", () => {
    if (location.protocol === "http:") document.getElementById("logout").hidden = true;
    const username = document.getElementById("identity")?.textContent;
    if (username) send({ kind: "authenticated", username });
    const navigation = document.getElementById("navigation");
    if (navigation && !document.getElementById("native-pair-device")) {
      const button = document.createElement("button");
      button.id = "native-pair-device";
      button.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="8" cy="8" r="5"/><path d="m11.5 11.5 9 9m-3-3 3-3m-6 0 3-3"/></svg>';
      button.title = "Pair another device with this host";
      button.setAttribute("aria-label", "Pair device");
      button.addEventListener("click", () => send({ kind: "pairDevice" }));
      navigation.append(button);
    }
  });
  document.addEventListener("DOMContentLoaded", () => {
    const style = document.createElement("style");
    style.textContent = (isMac ? `
      #sidebar .sidebar-brand { padding-left: 76px; }
      #sidebar .sidebar-brand strong { font-size: 0; }
      #sidebar .brand-mark { display: none; }
      #host-view { padding-left: 222px; }
      #sidebar { left: 2px; transform: translateX(calc(-100% - 2px)); }
      #host-view.sidebar-open #sidebar { transform: translateX(0); visibility: visible; }
      #host-view.sidebar-hidden { padding-left: 2px !important; }
      #host-view.sidebar-hidden #session-workspace > .tab-strip { padding-left: 80px; }
      #sidebar-toggle { top: 32px; width: 2px; }
      #notice:not([hidden]) { padding-top: 32px; }
      #session-workspace > .tab-strip { -webkit-app-region: drag; }
    ` : `
      :root { --xedoc-controls-width: calc(100vw - env(titlebar-area-x, 0px) - env(titlebar-area-width, 100vw)); }
      #session-workspace > .tab-strip {
        padding-left: max(4px, env(titlebar-area-x, 0px));
        padding-right: max(4px, var(--xedoc-controls-width));
        -webkit-app-region: drag;
      }
    `) + `
      #session-workspace > .tab-strip > *, #sidebar .sidebar-brand button { -webkit-app-region: no-drag; }
      #sidebar .sidebar-brand { -webkit-app-region: drag; }
      #notice:not([hidden]) { padding-right: max(12px, var(--xedoc-controls-width)); }
      body > :not(#host-view):not(#notice) { -webkit-app-region: no-drag; }
    `;
    document.head.append(style);
    const host = document.getElementById("host-view");
    const changed = () => { if (host?.hidden) send({ kind: "signedOut" }); };
    if (host) new MutationObserver(changed).observe(host, {
      attributes: true, attributeFilter: ["hidden"],
    });
  }, { once: true });
  document.addEventListener("click", (event) => {
    const candidate = event.target.closest("a.host-candidate");
    if (!candidate && !event.target.closest("#host-name-button")) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    send(candidate ? { kind: "openHost", url: candidate.href } : { kind: "hosts" });
  }, true);
  window.addEventListener("xedoc-activity-notification", (event) => send(event.detail));
}

// Isolated-world link routing for every frame, adapted from macos/Xedoc/WebsiteLinks.js.
// Ctrl (or Meta) opens the link in the external browser, matching Command on macOS.
function installWebsiteLinks(post) {
  document.addEventListener("click", (event) => {
    if (!event.isTrusted || event.button !== 0) return;
    const anchor = event.composedPath().find((node) => node instanceof HTMLAnchorElement);
    if (!anchor || anchor.hasAttribute("download")) return;
    let target;
    try { target = new URL(anchor.href); } catch { return; }
    if (!["http:", "https:"].includes(target.protocol)
        || target.origin === new URL(location.href).origin) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    post({
      url: target.href,
      external: event.ctrlKey || event.metaKey || anchor.classList.contains("plugin-panel-open"),
    });
  }, true);
}

const isMainFrame = window === window.top;
if (isMainFrame) {
  const restore = ipcRenderer.sendSync("xedoc:host-init");
  if (typeof restore === "string") {
    try {
      if (!sessionStorage.getItem("xedoc-host-view-v1")) sessionStorage.setItem("xedoc-host-view-v1", restore);
    } catch {}
  }
  contextBridge.exposeInMainWorld("webkit", {
    messageHandlers: { xedocDesktop: { postMessage: (body) => ipcRenderer.send("xedoc:desktop", body) } },
  });
  contextBridge.executeInMainWorld({ func: hostNativeScript, args: [process.platform === "darwin"] });
}
installWebsiteLinks((body) => ipcRenderer.send("xedoc:website", body));
