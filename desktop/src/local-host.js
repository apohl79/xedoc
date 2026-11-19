"use strict";

// Same-machine Host GUI access through $XEDOC_HOME/host-gui/local.json; no login or pairing needed.
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { validatedHostURL, canonicalOrigin } = require("./urls");

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
  return access && { hostname: "This computer — no setup required", url: access.url, route: "direct" };
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
      throw pairingError("Unexpected response from this computer’s host.");
    }
    if (typeof result.error === "string") throw pairingError(result.error.slice(0, 1024));
    if (response.status !== 200) {
      throw pairingError("Could not access this computer’s host. Reconnect using This computer in Hosts.");
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

module.exports = { localAccess, readLocalHost, LocalDevicePairing };
