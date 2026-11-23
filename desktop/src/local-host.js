"use strict";

// Same-machine Host GUI access through $XEDOC_HOME/host-gui/local.json; no login or pairing needed.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");
const crypto = require("node:crypto");
const { validatedHostURL, canonicalOrigin } = require("./urls");

function ensureWindowsLocalHostService() {
  if (process.platform !== "win32") return;
  const home = process.env.XEDOC_HOME || path.join(os.homedir(), ".xedoc");
  const resources = path.join(home, "packages", "standalone", "current", "xedoc-resources", "remote-agent");
  const python = path.join(resources, "runtime", "python", "python.exe");
  const payload = path.join(resources, "remote-agent.pyz");
  if (!fs.existsSync(python) || !fs.existsSync(payload)) return;
  const xedoc = path.join(home, "packages", "standalone", "current", "bin", "xedoc.exe");
  if (!fs.existsSync(xedoc)) return;

  const bootstrapOverride = process.env.XEDOC_REMOTE_AGENT_BOOTSTRAP;
  const bootstrap = bootstrapOverride || path.join(home, "remote-agent", "bootstrap.toml");
  if (bootstrapOverride && !fs.existsSync(bootstrap)) return;
  if (!bootstrapOverride && fs.existsSync(path.join(home, "remote-agent", "bootstrap-experimental.toml"))) return;
  const ensureBroker = () => {
    const child = spawn(
      python,
      [
        "-B",
        "-I",
        "-X",
        "utf8",
        payload,
        "daemon",
        "ensure",
        "--initialize-empty-config-if-fresh",
      ],
      {
        detached: true,
        stdio: "ignore",
        windowsHide: true,
      },
    );
    child.on("error", () => {});
    child.unref();
  };
  const start = spawn(xedoc, ["app-server", "daemon", "start"], {
    stdio: "ignore",
    windowsHide: true,
  });
  start.on("error", () => {});
  start.on("close", (code) => {
    if (code !== 0) {
      if (!fs.existsSync(bootstrap)) return;
      const probe = spawn(xedoc, ["app-server", "daemon", "version"], {
        stdio: "ignore",
        windowsHide: true,
      });
      probe.on("error", () => {});
      probe.on("close", (probeCode) => {
        if (probeCode === 0) ensureBroker();
      });
      return;
    }
    if (!fs.existsSync(bootstrap)) {
      const socket = path.join(home, "app-server-control", "app-server-control.sock");
      const directory = path.dirname(bootstrap);
      let temporary;
      try {
        fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
        temporary = path.join(directory, `.bootstrap-${crypto.randomUUID()}.tmp`);
        const descriptor = fs.openSync(temporary, "wx", 0o600);
        try {
          fs.writeFileSync(descriptor, `controller = ${JSON.stringify(`unix://${socket}`)}\n`);
          fs.fsyncSync(descriptor);
        } finally {
          fs.closeSync(descriptor);
        }
        try {
          fs.linkSync(temporary, bootstrap);
        } catch (error) {
          if (error.code !== "EEXIST") throw error;
        }
      } catch (error) {
        return;
      } finally {
        if (temporary) {
          try {
            fs.unlinkSync(temporary);
          } catch {}
        }
      }
    }
    ensureBroker();
  });
}

function localAccess() {
  const home = process.env.XEDOC_HOME || path.join(os.homedir(), ".xedoc");
  try {
    const data = fs.readFileSync(path.join(home, "host-gui", "local.json"));
    if (data.length > 1024) return null;
    const value = JSON.parse(data.toString("utf8"));
    const url = validatedHostURL(value?.origin);
    const token = value?.token;
    if (!url || url.protocol !== "http:" || url.hostname !== "127.0.0.1") return null;
    if (typeof token !== "string" || token.length !== 64 || !/^[\x2d-\x7a]+$/.test(token)) return null;
    return { url: url.href, token };
  } catch {
    return null;
  }
}

function readLocalHost() {
  const access = localAccess();
  const name = process.platform === "darwin" ? "This Mac" : "This computer";
  return access && { hostname: `${name} — no setup required`, url: access.url, route: "direct" };
}

function pairingError(message) {
  return new Error(message);
}

// Port of LocalDevicePairing.swift: same-machine administrator access for the Pair Device flow.
class LocalDevicePairing {
  constructor() {
    this.authenticatedToken = null;
    this.cookie = "";
  }

  async request(hostURL, apiPath, body) {
    const local = localAccess();
    if (!local) return null;
    const connectedOrigin = canonicalOrigin(hostURL);
    try {
      if (this.authenticatedToken !== local.token) {
        const response = await fetch(local.url, {
          headers: { Authorization: `Bearer ${local.token}` },
          redirect: "error",
          signal: AbortSignal.timeout(15_000),
        });
        await response.arrayBuffer();
        if (response.status !== 200) return null;
        const cookie = response.headers.getSetCookie().find((value) => value.startsWith("xedoc_host="));
        this.cookie = cookie ? cookie.split(";")[0] : "";
        this.authenticatedToken = local.token;
      }
      const protection = await this.send(local.url, "device/pairing", null);
      if (connectedOrigin !== canonicalOrigin(local.url) && connectedOrigin !== protection.origin) return null;
      if (apiPath === "device/pairing" && body === null) return protection;
    } catch (error) {
      if (connectedOrigin === canonicalOrigin(local.url)) throw error;
      return null;
    }
    return this.send(local.url, apiPath, body);
  }

  async send(origin, apiPath, body) {
    const headers = this.cookie ? { Cookie: this.cookie } : {};
    if (body !== null) {
      headers.Origin = canonicalOrigin(origin);
      headers["Content-Type"] = "application/json";
    }
    const response = await fetch(new URL(`api/${apiPath}`, origin), {
      method: body === null ? "GET" : "POST",
      headers,
      body: body === null ? undefined : JSON.stringify(body),
      redirect: "error",
      signal: AbortSignal.timeout(15_000),
    });
    const text = await readBounded(response, 16_384);
    let result;
    try {
      result = JSON.parse(text);
    } catch {
      result = null;
    }
    if (!result || typeof result !== "object" || Array.isArray(result)) {
      throw pairingError(process.platform === "darwin"
        ? "Unexpected response from this Mac’s host." : "Unexpected response from this computer’s host.");
    }
    if (typeof result.error === "string") throw pairingError(result.error.slice(0, 1024));
    if (response.status !== 200) {
      throw pairingError(process.platform === "darwin"
        ? "Could not access this Mac’s host. Reconnect using This Mac in Hosts."
        : "Could not access this computer’s host. Reconnect using This computer in Hosts.");
    }
    return result;
  }
}

async function readBounded(response, limit) {
  const chunks = [];
  let size = 0;
  for await (const chunk of response.body ?? []) {
    size += chunk.length;
    if (size > limit) throw pairingError("Pairing response is too large.");
    chunks.push(Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString("utf8");
}

module.exports = { ensureWindowsLocalHostService, localAccess, readLocalHost, LocalDevicePairing };
