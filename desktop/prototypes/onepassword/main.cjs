"use strict";

const fs = require("node:fs");
const { createHash } = require("node:crypto");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { app, BrowserWindow, Menu, session } = require("electron");
delete process.env.DEBUG;
require("debug").disable();
const { installNativeBridge } = require(app.isPackaged ? "./onepassword-native.js" : "../../src/onepassword-native.js");

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
    ["electron-chrome-extensions", "90411be233a92821b2873b9062584553d8a9f994a057f6efa34d32e267ee28ac"],
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
  let dummy;
  let inlineSetupRefreshed = false;
  const windows = new Set();
  const permittedURL = (url) => {
    if (dummyURL && url === dummyURL) return true;
    try {
      const parsed = new URL(url);
      if (parsed.username || parsed.password || parsed.port) return false;
      return (parsed.protocol === "chrome-extension:" && parsed.hostname === extensionId) ||
        (parsed.protocol === "https:" && ["1password.com", "1password.eu", "1password.ca"]
          .some((domain) => parsed.hostname === domain || parsed.hostname.endsWith(`.${domain}`)));
    } catch {
      return false;
    }
  };
  browser.webRequest.onBeforeRequest({ urls: ["<all_urls>"] }, (details, callback) => {
    callback({ cancel: details.resourceType === "mainFrame" && !permittedURL(details.url) });
  });
  const createWindow = async (url, title) => {
    if (!permittedURL(url)) throw new Error("Only the dummy page, official extension UI, and 1Password account sites are allowed.");
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
    const refreshInlineAfterSetup = () => {
      if (inlineSetupRefreshed || !dummy || dummy.isDestroyed()) return;
      if (window.webContents.getURL() !==
          `chrome-extension://${extensionId}/app/app.html#/page/ready`) return;
      inlineSetupRefreshed = true;
      dummy.webContents.reload();
    };
    window.webContents.on("did-finish-load", refreshInlineAfterSetup);
    window.webContents.on("did-navigate-in-page", refreshInlineAfterSetup);
    window.webContents.setWindowOpenHandler((details) => {
      if (permittedURL(details.url)) {
        createWindow(details.url, "1Password account sign-in").catch(() => {
          console.error("ACCOUNT WINDOW FAILED");
        });
      }
      return { action: "deny" };
    });
    window.webContents.on("will-navigate", (event) => {
      if (!permittedURL(event.url)) event.preventDefault();
    });
    window.webContents.on("will-redirect", (event) => {
      if (event.isMainFrame && !permittedURL(event.url)) event.preventDefault();
    });
    extensions.addTab(window.webContents, window);
    try {
      await window.loadURL(url);
    } catch (error) {
      window.destroy();
      throw error;
    }
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
  const extensionPath = path.join(profile, "extension");
  fs.cpSync(path.join(__dirname, "extension"), extensionPath, { recursive: true, errorOnExist: true, force: false });
  const extension = await browser.extensions.loadExtension(extensionPath);
  if (extension.id !== extensionId) throw new Error("Unexpected extension identity.");
  console.log("OFFICIAL EXTENSION", extension.id, extension.version, "Electron", process.versions.electron);
  const server = http.createServer((_request, response) => {
    response.setHeader("Content-Type", "text/html; charset=utf-8");
    response.setHeader("Content-Security-Policy", "default-src 'none'; form-action 'none'; style-src 'unsafe-inline'");
    response.end(`<!doctype html><title>Xedoc dummy login</title>
      <style>body{font:18px system-ui;padding:24px;max-width:680px}label{display:block;margin:12px 0}input{font:inherit}code{user-select:all}</style>
      <h1>1Password compatibility probe</h1>
      <p>This separate experiment never changes normal Xedoc browser sessions.</p>
      <p>Use <b>1Password → Open 1Password</b> (Cmd+Shift+P).</p>
      <p>For inline suggestions, click a field or use
      <b>1Password → Show autofill suggestions</b> (Cmd+P).
      Use arrow keys to select, Enter to choose, and Escape to dismiss.</p>
      <p>Approve browser trust and unlock manually. Use only a disposable test login.
      This form cannot submit. Close all probe windows to end the experiment.</p>
      <p>Set the disposable Login's <b>website</b> to <code>${dummyURL}</code>.
      Return to this window, open 1Password, then choose <b>Autofill</b> for that login.
      Approve 1Password's HTTP confirmation if prompted.</p>
      <p>Searching a real site's login does not make it match this dummy page.
      <b>Open &amp; Fill</b> to other sites is blocked. Normal Xedoc panels are not
      connected to this experiment.</p>
      <p>Account sign-in opens inside this probe on official 1Password sites only.
      External SSO and unrelated pop-ups are blocked. This temporary session quits after
      ten minutes and does not retain your account on normal exit.</p>
      <label>Username <input autocomplete=username name=username></label>
      <label>Password <input type=password autocomplete=current-password name=password></label>
      <button type=button>Dummy sign in</button>`);
  });
  server.on("error", () => app.quit());
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  app.once("before-quit", () => server.close());
  dummyURL = `http://127.0.0.1:${server.address().port}/`;
  dummy = await createWindow(dummyURL, "Xedoc dummy login");
  extensions.selectTab(dummy.webContents);
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: "1Password", submenu: [
      { label: "Show autofill suggestions", accelerator: "CommandOrControl+P", click: async () => {
        if (dummy.isDestroyed() || dummy.webContents.getURL() !== dummyURL) return;
        try {
          dummy.show();
          dummy.focus();
          dummy.webContents.focus();
          const focused = await dummy.webContents.executeJavaScript(`(() => {
            const field = document.activeElement instanceof HTMLInputElement
              ? document.activeElement : document.querySelector("input[name=username]");
            field?.focus();
            return !!field;
          })()`);
          if (!focused || dummy.isDestroyed() || dummy.webContents.getURL() !== dummyURL) return;
          dummy.webContents.sendInputEvent({ type: "keyDown", keyCode: "Down" });
          dummy.webContents.sendInputEvent({ type: "keyUp", keyCode: "Down" });
        } catch {
          console.error("INLINE SUGGESTIONS FAILED");
        }
      } },
      { label: "Reload test page", accelerator: "CommandOrControl+R", click: () => {
        if (!dummy.isDestroyed()) dummy.webContents.reload();
      } },
      { type: "separator" },
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
