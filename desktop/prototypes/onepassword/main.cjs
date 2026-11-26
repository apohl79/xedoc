"use strict";

const fs = require("node:fs");
const { createHash } = require("node:crypto");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { app, BrowserWindow, Menu, session } = require("electron");
delete process.env.DEBUG;
require("debug").disable();
const { installNativeBridge } = require("./native-host.cjs");

const extensionId = "aeblfdkhhhdcdjpifhhbdiojplfjncoa";
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "xedoc-onepassword-profile-"));
app.setName("Xedoc 1Password Probe");
app.setPath("userData", profile);
app.setPath("sessionData", profile);
app.on("window-all-closed", () => app.quit());
app.once("quit", () => {
  try {
    fs.rmSync(profile, { recursive: true, force: true, maxRetries: 3 });
  } catch {
    console.error("TEMPORARY PROFILE CLEANUP FAILED");
  }
});
setTimeout(() => app.quit(), 600_000).unref();
function fail(error) {
  const frames = String(error?.stack).match(/(?:main\.cjs|native-host\.cjs|index\.js):\d+:\d+/g) || [];
  console.error("PROBE FAILED", frames.slice(0, 5).join(" "));
  app.exit(1);
}
process.on("uncaughtException", fail);

app.whenReady().then(async () => {
  for (const [module, digest] of [
    ["electron-chrome-extensions", "c8c50a30c29aec4191c58d3f53524f766f27c9f5ed43c6a28c18f932738729ab"],
    ["electron-chrome-extensions/preload", "7d763b069204552a7145fc52d7c6576ae13785c1ff3d24673eab1542bad043c3"],
  ]) {
    const actual = createHash("sha256").update(fs.readFileSync(require.resolve(module))).digest("hex");
    if (actual !== digest) {
      console.error("DEPENDENCY SAFETY PATCH MISSING");
      throw new Error("Unsafe extension runtime.");
    }
  }
  const { ElectronChromeExtensions } = require("electron-chrome-extensions");
  const browser = session.fromPartition("persist:onepassword");
  browser.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  browser.setPermissionCheckHandler(() => false);
  browser.on("will-download", (event) => event.preventDefault());
  let extensions;
  let dummyURL;
  const windows = new Set();
  const permittedURL = (url) => {
    const parsed = new URL(url);
    return url === dummyURL ||
      (parsed.protocol === "chrome-extension:" && parsed.hostname === extensionId);
  };
  const createWindow = async (url, title) => {
    if (!permittedURL(url)) throw new Error("Only the dummy page and official extension UI are allowed.");
    const window = new BrowserWindow({
      title, width: 800, height: 650, show: false,
      webPreferences: {
        session: browser, sandbox: true, contextIsolation: true,
        backgroundThrottling: false, devTools: false,
      },
    });
    windows.add(window);
    window.on("closed", () => windows.delete(window));
    window.on("page-title-updated", (event) => event.preventDefault());
    window.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
    window.webContents.on("will-navigate", (event) => {
      if (!permittedURL(event.url)) event.preventDefault();
    });
    extensions.addTab(window.webContents, window);
    await window.loadURL(url);
    window.showInactive();
    return window;
  };
  extensions = new ElectronChromeExtensions({
    session: browser,
    requestPermissions: async () => false,
    createTab: async (details) => {
      const window = await createWindow(details.url, "1Password setup probe");
      console.log("EXTENSION TAB OPENED");
      return [window.webContents, window];
    },
    selectTab: (contents, window) => {
      extensions.selectTab(contents);
      window.showInactive();
    },
    removeTab: (contents) => {
      if (!contents.isDestroyed()) contents.close();
    },
  });
  installNativeBridge(extensions);
  browser.serviceWorkers.on("console-message", (_event, details) => {
    if (details.message.includes("Finished initializing 1Password")) console.log("BACKGROUND READY");
    const state = details.message.match(/Desktop connection changed from \w+ to (Connecting|Disconnected|Connected)(?:, reason: (\w+))?/);
    if (state) {
      const reasons = new Set(["PortClosed", "NativeHostNotFound", "BrowserSignatureInvalid", "UnknownBrowser", "ExtensionSupportDisabled", "MainAppNotRunning"]);
      console.log("1PASSWORD DESKTOP STATE", state[1], reasons.has(state[2]) ? state[2] : "");
    }
  });
  const extension = await browser.extensions.loadExtension(path.join(__dirname, "extension"));
  if (extension.id !== extensionId) throw new Error("Unexpected extension identity.");
  console.log("OFFICIAL EXTENSION", extension.id, extension.version, "Electron", process.versions.electron);
  const server = http.createServer((_request, response) => {
    response.setHeader("Content-Type", "text/html; charset=utf-8");
    response.setHeader("Content-Security-Policy", "default-src 'none'; form-action 'none'; style-src 'unsafe-inline'");
    response.end(`<!doctype html><title>Xedoc dummy login</title>
      <style>body{font:18px system-ui;padding:32px;max-width:640px}label{display:block;margin:16px 0}input{font:inherit}</style>
      <h1>1Password compatibility probe</h1>
      <p>This separate experiment never changes normal Xedoc browser sessions.</p>
      <p>Use <b>1Password → Open 1Password</b> (Cmd+Shift+P).</p>
      <p>Approve browser trust and unlock manually. Use only a disposable test login.
      This form cannot submit. Close all probe windows to end the experiment.</p>
      <label>Username <input autocomplete=username name=username></label>
      <label>Password <input type=password autocomplete=current-password name=password></label>
      <button type=button>Dummy sign in</button>`);
  });
  server.on("error", () => app.quit());
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  app.once("before-quit", () => server.close());
  dummyURL = `http://127.0.0.1:${server.address().port}/`;
  const dummy = await createWindow(dummyURL, "Xedoc dummy login");
  extensions.selectTab(dummy.webContents);
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: "1Password", submenu: [
      { label: "Open 1Password", accelerator: "CommandOrControl+Shift+P", click: () => {
        if (dummy.isDestroyed()) return;
        extensions.selectTab(dummy.webContents);
        console.log("ACTION CLICK");
        extensions.api.browserAction.activateClick({
          extensionId, tabId: dummy.webContents.id, anchorRect: { x: 740, y: 40, width: 32, height: 32 },
        });
      } },
      { role: "quit" },
    ] },
  ]));
}).catch(fail);
