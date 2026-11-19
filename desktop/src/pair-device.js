"use strict";

// Main-process side of PairDeviceView.swift; the dialog page (ui/pair-device.html) owns the steps.
const { clipboard } = require("electron");
const pairing = require("./pairing");
const { LocalDevicePairing } = require("./local-host");
const { canonicalOrigin } = require("./urls");

const paths = new Set(["device/pairing", "device/bridge-setup", "device/invite", "device/pairing-status"]);

// Same request the macOS app evaluates in the signed-in host page when this is not the local host.
function pageRequest(origin, apiPath, body) {
  return `(async () => {
    const origin = ${JSON.stringify(origin)}, path = ${JSON.stringify(apiPath)}, body = ${JSON.stringify(body)};
    if (location.origin !== origin) throw new Error("Open and sign in to the host first.");
    const response = await fetch("/api/" + path, {
      method: body === null ? "GET" : "POST",
      credentials: "same-origin", redirect: "error",
      headers: body === null ? {} : {"Content-Type": "application/json"},
      body: body === null ? undefined : body,
      signal: AbortSignal.timeout(15000)
    });
    const text = await response.text();
    if (text.length > 16384) throw new Error("Pairing response is too large.");
    if (!response.ok) {
      let message;
      try { message = JSON.parse(text).error; } catch {}
      return JSON.stringify({error: message || "Sign in as an administrator and update the host if needed."});
    }
    return text;
  })()`;
}

function handlers(connection) {
  const local = new LocalDevicePairing();
  const request = async (apiPath, body) => {
    if (!paths.has(apiPath) || (body !== null && (typeof body !== "object" || Array.isArray(body)))) {
      throw new Error("Unsupported pairing request.");
    }
    const result = await local.request(connection.url, apiPath, body);
    if (result) return result;
    const text = await connection.contents.executeJavaScript(
      pageRequest(canonicalOrigin(connection.url), apiPath, body === null ? null : JSON.stringify(body)), true);
    let value;
    try {
      value = JSON.parse(text);
    } catch {
      value = null;
    }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      throw new Error("Unexpected response. Update the host if needed.");
    }
    if (typeof value.error === "string") throw new Error(value.error.slice(0, 1024));
    return value;
  };
  return {
    origin: () => canonicalOrigin(connection.url),
    request: async (apiPath, body = null) => {
      try {
        return { result: await request(apiPath, body) };
      } catch (error) {
        return { error: error?.message ?? String(error) };
      }
    },
    preview: (text) => {
      try {
        const { origin, fingerprint } = pairing.parseInvitation(String(text ?? ""));
        return { origin, fingerprint };
      } catch (error) {
        return { error: error.message };
      }
    },
    copy: (text) => clipboard.writeText(String(text ?? "")),
  };
}

module.exports = { handlers };
