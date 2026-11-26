"use strict";

const { spawn, execFile } = require("node:child_process");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { promisify } = require("node:util");
const { app } = require("electron");

const extensionId = "aeblfdkhhhdcdjpifhhbdiojplfjncoa";
const origin = `chrome-extension://${extensionId}/`;
const application = "com.1password.1password";
const helper = "/Applications/1Password.app/Contents/Library/LoginItems/1Password Browser Helper.app/Contents/MacOS/1Password-BrowserSupport";
const limit = 1024 * 1024;
const execute = promisify(execFile);

async function validateHelper() {
  if (process.platform !== "darwin") throw new Error("This probe's native bridge is macOS-only.");
  const configPath = path.join(os.homedir(), "Library/Application Support/Google/Chrome/NativeMessagingHosts", `${application}.json`);
  const stat = await fs.stat(configPath);
  if (stat.size > 16_384) throw new Error("Invalid native host registration.");
  const config = JSON.parse(await fs.readFile(configPath, "utf8"));
  if (config.name !== application || config.type !== "stdio" ||
      config.path !== helper || !config.allowed_origins?.includes(origin)) {
    throw new Error("Unexpected native host registration.");
  }
  await execute("/usr/bin/codesign", [
    "--verify", "--strict", "-R",
    '=anchor apple generic and certificate leaf[subject.OU] = "2BUA8C4S2C"',
    helper,
  ], { timeout: 10_000, maxBuffer: 16_384 });
}

function installNativeBridge(extensions) {
  const connections = new Map();
  const workers = extensions.ctx.session.serviceWorkers;
  let attempts = 0;
  const handle = extensions.ctx.router.apiHandler();
  const authenticate = (event, connectionId) => {
    if (event.extension?.id !== extensionId || event.type !== "service-worker" ||
        event.sender.scope !== origin || !/^[a-f0-9-]{36}$/.test(connectionId)) {
      throw new Error("Native messaging is restricted to the official 1Password service worker.");
    }
  };
  handle("runtime.connectNative", async (event, connectionId, name) => {
    authenticate(event, connectionId);
    if (name !== application || connections.has(connectionId) ||
        connections.size >= 2 || attempts >= 2) {
      event.sender.send(`crx-native-msg-${connectionId}-disconnect`);
      return;
    }
    attempts += 1;
    const sender = event.sender;
    const channel = `crx-native-msg-${connectionId}`;
    let child;
    let closed = false;
    let incoming = Buffer.alloc(0);
    let pending = [];
    let pendingBytes = 0;
    let received = false;
    let keepAlive;
    const timeout = setTimeout(() => close("handshake-timeout"), 30_000);
    const close = (reason) => {
      if (closed) return;
      closed = true;
      clearTimeout(timeout);
      connections.delete(connectionId);
      sender.ipc.off(channel, send);
      workers.off("running-status-changed", onStatus);
      try { keepAlive?.end(); } catch {}
      pending = [];
      incoming = Buffer.alloc(0);
      child?.kill();
      try { sender.send(`${channel}-disconnect`); } catch {}
      console.log("NATIVE BRIDGE CLOSED", reason);
    };
    const onStatus = ({ versionId, runningStatus }) => {
      if (versionId === sender.versionId && (runningStatus === "stopping" || runningStatus === "stopped")) {
        close("worker-stopped");
      }
    };
    const send = (_event, message) => {
      if (closed) return;
      let body;
      try { body = Buffer.from(JSON.stringify(message), "utf8"); } catch { return close("invalid-outgoing-json"); }
      if (!body.length || body.length > limit) return close("outgoing-size-limit");
      const frame = Buffer.alloc(4 + body.length);
      frame.writeUInt32LE(body.length);
      body.copy(frame, 4);
      if (!child) {
        pendingBytes += frame.length;
        if (pending.length >= 16 || pendingBytes > limit) return close("pending-limit");
        pending.push(frame);
      } else if (child.stdin.writableLength + frame.length > limit) {
        close("backpressure-limit");
      } else {
        child.stdin.write(frame);
      }
    };
    connections.set(connectionId, { sender, close });
    sender.ipc.on(channel, send);
    workers.on("running-status-changed", onStatus);
    try {
      keepAlive = sender.startTask();
      await validateHelper();
      if (closed) return;
      child = spawn(helper, [origin], {
        shell: false,
        stdio: ["pipe", "pipe", "ignore"],
        env: { HOME: os.homedir(), PATH: "/usr/bin:/bin:/usr/sbin:/sbin", TMPDIR: app.getPath("temp") },
      });
      child.once("error", () => close("launch-error"));
      child.once("exit", () => close("host-exited"));
      child.stdin.on("error", () => close("pipe-error"));
      child.stdout.on("error", () => close("pipe-error"));
      child.stdout.on("data", (data) => {
        if (closed) return;
        incoming = Buffer.concat([incoming, data]);
        while (incoming.length >= 4) {
          const length = incoming.readUInt32LE(0);
          if (!length || length > limit) return close("incoming-size-limit");
          if (incoming.length < length + 4) break;
          const body = incoming.subarray(4, length + 4);
          incoming = incoming.subarray(length + 4);
          let message;
          try { message = JSON.parse(body.toString("utf8")); } catch { return close("invalid-incoming-json"); }
          clearTimeout(timeout);
          try { sender.send(channel, message); } catch { return close("worker-unavailable"); }
          if (!received) {
            received = true;
            const types = new Set(["Success", "Failure", "BrowserSupport", "Notification", "NotificationModern"]);
            const type = types.has(message?.type) ? message.type : "other";
            console.log("NATIVE BRIDGE FIRST RESPONSE", type);
            const statuses = new Set([
              "MainAppNotRunning", "MainAppOpenTimeout", "CannotOpenMainApp", "UnknownInvocation",
              "BrowserHelperNotRegistered", "PartnerBrowserAuthorizationSuccess", "PartnerBrowserAuthorizationFailure",
              "ExtensionSupportDisabled", "PortClosed", "HostNotFound", "Timeout",
              "AppQuit", "BrowserVerificationFailed", "UnknownBrowser", "BrowserSignatureInvalid",
            ]);
            let status;
            if (type === "BrowserSupport") status = message.content?.response;
            if (type === "Failure") status = message.content?.response?.type || message.content?.response?.reason;
            if (type === "Notification" || type === "NotificationModern") {
              const notification = type === "Notification" ? message.content : message.content?.content;
              status = notification?.type === "BrowserVerificationFailed" ? notification.content : notification?.type;
            }
            if (type !== "Success") console.log("1PASSWORD HELPER STATUS", statuses.has(status) ? status : "other");
          }
        }
        if (incoming.length > limit + 4) close("incoming-buffer-limit");
      });
      for (const frame of pending) child.stdin.write(frame);
      pending = [];
      pendingBytes = 0;
      console.log("NATIVE BRIDGE HOST LAUNCHED");
    } catch {
      close("host-validation-failed");
    }
  }, { permission: "nativeMessaging" });
  handle("runtime.disconnectNative", (event, connectionId) => {
    authenticate(event, connectionId);
    const connection = connections.get(connectionId);
    if (connection?.sender === event.sender) connection.close("extension-disconnected");
  }, { permission: "nativeMessaging" });
  handle("runtime.sendNativeMessage", () => {
    throw new Error("This 1Password probe only supports persistent native ports.");
  }, { permission: "nativeMessaging" });
  app.once("before-quit", () => {
    for (const connection of connections.values()) connection.close("probe-quit");
  });
}

module.exports = { installNativeBridge };
