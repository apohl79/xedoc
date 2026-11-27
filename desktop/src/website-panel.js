"use strict";

// Port of WebsitePanel.swift: a browser view the host page positions over its website panel.
const { WebContentsView, session } = require("electron");
const { validatedWebsiteURL } = require("./urls");
const { BrowserDevtools } = require("./browser-devtools");
const { BrowserDownloads } = require("./browser-downloads");
const extensions = require("./website-extensions");

class WebsitePanel {
  constructor({ changed, openNew, window, activate }) {
    // Panels share the persistent default session so logins carry across panels and launches.
    this.view = new WebContentsView({
      webPreferences: { session: session.defaultSession, sandbox: true, contextIsolation: true, backgroundThrottling: false },
    });
    // Keep a rendered viewport outside the window: hiding it disables Chromium input.
    this.view.setBounds({ x: -1281, y: 0, width: 1280, height: 800 });
    this.view.setVisible(true);
    this.title = "Website";
    this.address = "";
    this.error = null;
    this.changed = changed;
    this.openNew = openNew;
    const contents = this.view.webContents;
    this.devtools = new BrowserDevtools(contents);
    this.downloads = new BrowserDownloads(contents);
    contents.on("page-title-updated", (_event, title) => {
      this.title = title || hostname(contents.getURL()) || "Website";
      this.changed();
    });
    const navigated = (_event, url) => {
      this.address = url;
      this.changed();
    };
    contents.on("did-navigate", navigated);
    contents.on("did-navigate-in-page", navigated);
    contents.on("did-start-loading", () => this.changed());
    contents.on("did-stop-loading", () => this.changed());
    contents.on("did-finish-load", () => {
      // Initialize Chromium's hidden rendering surface after navigation, without focusing the view.
      contents.backgroundThrottling = false;
      this.error = null;
      this.changed();
    });
    contents.on("did-fail-load", (_event, code, description, _url, isMainFrame) => {
      if (!isMainFrame || code === -3) return;
      this.error = describeLoadError(code, description);
      this.changed();
    });
    contents.on("will-navigate", (event) => {
      if (!validatedWebsiteURL(event.url)) event.preventDefault();
      else this.error = null;
    });
    contents.on("will-redirect", (event) => {
      if (event.isMainFrame && !validatedWebsiteURL(event.url)) event.preventDefault();
    });
    contents.setWindowOpenHandler(({ url, disposition }) => {
      if (validatedWebsiteURL(url)) {
        this.openNew(url, disposition === "background-tab");
      }
      return { action: "deny" };
    });
    extensions.registerPanel(contents, window, activate);
  }

  state(id) {
    const history = this.view.webContents.navigationHistory;
    return {
      id, title: this.title, address: this.address,
      canGoBack: history.canGoBack(), canGoForward: history.canGoForward(), error: this.error,
      loading: this.view.webContents.isLoadingMainFrame(),
    };
  }

  load(url) {
    if (!validatedWebsiteURL(url)) return;
    this.error = null;
    this.address = url;
    this.title = hostname(url) || "Website";
    this.view.webContents.loadURL(url).catch(() => {});
  }

  submitAddress(text) {
    const value = String(text ?? "").trim();
    const url = validatedWebsiteURL(value.includes("://") ? value : `https://${value}`);
    this.address = value;
    if (!url) {
      this.error = "Enter an HTTP or HTTPS website address.";
      return;
    }
    this.load(url.href);
  }

  close() {
    this.view.webContents.close({ waitForBeforeUnload: false });
  }
}

function hostname(url) {
  try {
    return new URL(url).hostname;
  } catch {
    return "";
  }
}

const loadErrors = {
  "-105": "A server with the specified hostname could not be found.",
  "-106": "The internet connection appears to be offline.",
  "-102": "Could not connect to the server.",
  "-118": "The request timed out.",
  "-7": "The request timed out.",
  "-109": "The address is unreachable.",
  "-101": "The network connection was lost.",
  "-100": "The network connection was lost.",
};

function describeLoadError(code, description) {
  return loadErrors[code] ?? `The page could not be loaded (${description || code}).`;
}

module.exports = { WebsitePanel, describeLoadError };
