"use strict";

// Port of DiscoveredHost.validatedURL (ios/Xedoc/HostDiscovery.swift): a host origin is an
// HTTPS origin, or HTTP for loopback only, without credentials, path, query, or fragment.
function validatedHostURL(origin) {
  if (typeof origin !== "string" || origin.length > 512 || !/^[\x21-\x7e]+$/.test(origin)) return null;
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\/([^/?#]*)(\/?)$/.exec(origin);
  if (!match || match[2].includes("@")) return null;
  let url;
  try {
    url = new URL(origin);
  } catch {
    return null;
  }
  if (!url.hostname || url.username || url.password) return null;
  const port = /:(\d+)$/.exec(match[2]);
  if (port && !(Number(port[1]) >= 1 && Number(port[1]) <= 65535)) return null;
  if (url.protocol !== "https:") {
    if (url.protocol !== "http:" || !["127.0.0.1", "localhost", "[::1]"].includes(url.hostname.toLowerCase())) {
      return null;
    }
  }
  return url;
}

// Port of WebsitePanel.validatedURL: any HTTP(S) URL with a host and no credentials.
function validatedWebsiteURL(text) {
  if (typeof text !== "string" || Buffer.byteLength(text) > 16_384) return null;
  let url;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if (!["http:", "https:"].includes(url.protocol) || !url.hostname || url.username || url.password) return null;
  return url;
}

// Canonical origin string matching HostDevicePairing.origin: lowercase host, no default 443 port.
function canonicalOrigin(url) {
  const parsed = new URL(url);
  const port = parsed.port && !(parsed.protocol === "https:" && parsed.port === "443") ? `:${parsed.port}` : "";
  return `${parsed.protocol}//${parsed.hostname.toLowerCase()}${port}`;
}

function sameOrigin(left, right) {
  try {
    return new URL(left).origin === new URL(right).origin;
  } catch {
    return false;
  }
}

module.exports = { validatedHostURL, validatedWebsiteURL, canonicalOrigin, sameOrigin };
