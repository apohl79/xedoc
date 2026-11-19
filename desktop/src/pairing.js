"use strict";

// Port of HostDevicePairing.swift: bridge invitations, device enrollment, pinned host trust, and the
// device client identity. Paired sessions reach their host through bridge-proxy.js.
const crypto = require("node:crypto");
const fs = require("node:fs");
const http = require("node:http");
const os = require("node:os");
const path = require("node:path");
const { app, safeStorage } = require("electron");
const {
  BridgeProxy, connectPinned, identityFailure, timeoutError, bareHostname, verifyPairedHost,
} = require("./bridge-proxy");
const { validatedHostURL, canonicalOrigin } = require("./urls");

const HOST_LIMIT = 128;
const proxies = new Map();
const identities = new Map();

function base64(value) {
  if (typeof value !== "string" || value.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(value)) return null;
  return Buffer.from(value, "base64");
}

function isCertificate(der) {
  try {
    return new crypto.X509Certificate(der).raw.equals(der);
  } catch {
    return false;
  }
}

function isStringMap(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    && Object.values(value).every((item) => typeof item === "string");
}

function rawPublicKey(key) {
  const jwk = key.export({ format: "jwk" });
  if (jwk.crv !== "P-256") return Buffer.alloc(0);
  return Buffer.concat([Buffer.from([4]), Buffer.from(jwk.x, "base64url"), Buffer.from(jwk.y, "base64url")]);
}

function parseInvitation(text) {
  const encoded = String(text ?? "").trim();
  if (encoded.length > 16_384) throw new Error("Invitation is too large.");
  if (!/^[A-Za-z0-9+/_-]*={0,2}$/.test(encoded) || encoded.length % 4 === 1) {
    throw new Error("Paste the invitation from your trusted host.");
  }
  let value;
  try {
    value = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8"));
  } catch {
    throw new Error("Invalid host invitation.");
  }
  const invitation = value !== null && typeof value === "object" && !Array.isArray(value) ? {
    version: value.version,
    origin: value.origin,
    serverCA: base64(value.serverCA),
    serverCertificate: base64(value.serverCertificate),
    token: value.token,
    route: value.route ?? null,
  } : null;
  if (!invitation || !Number.isInteger(invitation.version) || typeof invitation.origin !== "string"
      || !invitation.serverCA || !invitation.serverCertificate || typeof invitation.token !== "string"
      || ![null, "direct", "bridge"].includes(invitation.route)) {
    throw new Error("Invalid host invitation.");
  }
  if (invitation.route !== "bridge") {
    throw new Error("Create a new bridge invitation. Directly reachable hosts only need sign-in.");
  }
  const url = validatedHostURL(invitation.origin);
  const tokenLength = Array.from(invitation.token).length;
  if (invitation.version !== 1 || url?.protocol !== "https:" || canonicalOrigin(url) !== invitation.origin
      || tokenLength < 32 || tokenLength > 128
      || !isCertificate(invitation.serverCA) || !isCertificate(invitation.serverCertificate)) {
    throw new Error("Invalid host invitation.");
  }
  const fingerprint = crypto.createHash("sha256").update(invitation.serverCertificate).digest("hex")
    .match(/../g).join(":");
  return { ...invitation, fingerprint };
}

function storePath() {
  return path.join(app.getPath("userData"), "paired-hosts.json");
}

function readStore() {
  let value;
  try {
    const text = fs.readFileSync(storePath(), "utf8");
    if (text.length > HOST_LIMIT * 32_768) throw new Error("Paired host store is too large.");
    value = JSON.parse(text);
  } catch (error) {
    if (error.code === "ENOENT") return [];
    throw new Error("Could not read this host’s pairing.", { cause: error });
  }
  if (value?.version !== 1 || !Array.isArray(value.hosts)) throw new Error("Could not read this host’s pairing.");
  return value.hosts;
}

function writeStore(hosts) {
  const file = storePath();
  const temporary = `${file}.${crypto.randomUUID()}.tmp`;
  try {
    fs.mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 });
    const descriptor = fs.openSync(temporary, "wx", 0o600);
    try {
      fs.writeFileSync(descriptor, JSON.stringify({ version: 1, hosts }));
      fs.fsyncSync(descriptor);
    } finally {
      fs.closeSync(descriptor);
    }
    fs.renameSync(temporary, file);
  } catch (error) {
    throw new Error("Could not save host trust.", { cause: error });
  } finally {
    fs.rmSync(temporary, { force: true });
  }
}

function decodeRecord(entry, origin) {
  const record = entry !== null && typeof entry === "object" ? {
    origin: entry.origin,
    serverCA: base64(entry.serverCA),
    serverCertificate: base64(entry.serverCertificate),
    certificate: base64(entry.certificate),
    privateKey: base64(entry.privateKey),
    protection: entry.protection,
    storeID: entry.storeID,
    route: entry.route ?? null,
  } : null;
  if (!record || record.origin !== origin || !record.serverCA || !record.serverCertificate || !record.certificate
      || !record.privateKey || !["safeStorage", "none"].includes(record.protection)
      || typeof record.storeID !== "string" || !/^[0-9a-f-]{36}$/.test(record.storeID)
      || ![null, "direct", "bridge"].includes(record.route)) {
    throw new Error("Invalid saved host pairing.");
  }
  return record;
}

function stored(url) {
  const origin = canonicalOrigin(url);
  const entry = readStore().find((item) => item?.origin === origin);
  return entry ? decodeRecord(entry, origin) : null;
}

function pairedHosts() {
  let entries;
  try {
    entries = readStore();
  } catch {
    return [];
  }
  return entries.slice(0, HOST_LIMIT).flatMap((entry) => {
    const url = validatedHostURL(entry?.origin);
    if (!url) return [];
    let route = "direct";
    try {
      route = decodeRecord(entry, canonicalOrigin(url)).route ?? "direct";
    } catch {
      // Like the Swift app, an unreadable record still lists its host; availableHost rejects it.
    }
    return [{ hostname: bareHostname(url.hostname), url: url.href, route }];
  });
}

async function identity(record) {
  const cached = identities.get(record.storeID);
  if (cached) return cached;
  try {
    const der = record.protection === "safeStorage"
      ? Buffer.from((await safeStorage.decryptStringAsync(record.privateKey)).result, "base64") : record.privateKey;
    const key = crypto.createPrivateKey({ key: der, format: "der", type: "pkcs8" });
    const certificate = new crypto.X509Certificate(record.certificate);
    if (!certificate.checkPrivateKey(key)) throw new Error("Device identity does not match the pairing.");
    const value = { cert: certificate.toString(), key: key.export({ type: "pkcs8", format: "pem" }) };
    identities.set(record.storeID, value);
    return value;
  } catch (error) {
    throw identityFailure(error);
  }
}

// One HTTP/1.1 request over a verified pinned connection; no redirects, cookies, or keep-alive.
// Resolves with a null body when the response exceeds `limit` bytes.
async function request(trust, { method = "GET", target, headers = {}, body, limit, idle, total = idle }) {
  const deadline = Date.now() + total;
  const url = new URL(target, trust.origin);
  const socket = await connectPinned(trust, Math.min(idle, total));
  return new Promise((resolve, reject) => {
    const outgoing = http.request({
      method, path: url.pathname, setHost: false, headers: { Host: url.host, ...headers },
      createConnection: () => socket,
    });
    const fail = (error) => {
      reject(error);
      outgoing.destroy(error);
    };
    const timer = setTimeout(() => fail(timeoutError()), Math.max(0, deadline - Date.now()));
    outgoing.setTimeout(idle, () => fail(timeoutError()));
    outgoing.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    outgoing.on("response", (response) => {
      const chunks = [];
      let size = 0;
      response.on("data", (chunk) => {
        size += chunk.length;
        if (size <= limit) {
          chunks.push(chunk);
          return;
        }
        clearTimeout(timer);
        resolve({ status: response.statusCode, body: null });
        socket.destroy();
      });
      response.on("end", () => {
        clearTimeout(timer);
        resolve({ status: response.statusCode, body: Buffer.concat(chunks) });
        socket.destroy();
      });
    });
    outgoing.end(body);
  });
}

async function availableHost(host) {
  try {
    const record = stored(host.url);
    if (!record) return null;
    const response = await request({ ...record, ...(await identity(record)) }, {
      target: "api/host-info", limit: 4096, idle: 3_000, total: 4_000,
    });
    const info = response.status === 200 && response.body ? JSON.parse(response.body.toString("utf8")) : null;
    const hostname = isStringMap(info) ? info.hostname : null;
    if (typeof hostname !== "string" || !hostname || Array.from(hostname).length > 255
        || /[\p{Cc}\p{Cf}]/u.test(hostname)) return null;
    return { hostname, url: host.url, route: record.route ?? "direct" };
  } catch {
    return null;
  }
}

async function pair(invitation, name) {
  const url = new URL(invitation.origin);
  const full = () => {
    const hosts = pairedHosts();
    return hosts.length >= HOST_LIMIT && !hosts.some((host) => host.url === url.href);
  };
  if (full()) throw new Error("Paired host limit reached.");
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicData = rawPublicKey(publicKey);
  const encodedKey = publicData.toString("base64");
  const device = Array.from(String(name).replace(/[\x00-\x1f]/g, " ")).slice(0, 64).join("");
  const proof = `xedoc-device-enroll-v1\n${invitation.origin}\n${invitation.token}\n${encodedKey}\n${device}`;
  const signature = crypto.sign("sha256", Buffer.from(proof), privateKey).toString("base64");
  let response;
  try {
    response = await request(invitation, {
      method: "POST",
      target: "api/device/enroll",
      headers: { Origin: invitation.origin, "Content-Type": "application/json" },
      body: JSON.stringify({ token: invitation.token, publicKey: encodedKey, signature, name: device }),
      limit: 16_384,
      idle: 15_000,
      total: 60_000,
    });
  } catch (error) {
    if (error.identityFailure) throw error;
    throw new Error(error.code === "ETIMEDOUT" ? "The request timed out." : "Could not connect to the server.",
      { cause: error });
  }
  if (!response.body) throw new Error("Pairing response is too large.");
  let certificate = null;
  try {
    const json = JSON.parse(response.body.toString("utf8"));
    const data = response.status === 200 && isStringMap(json) ? base64(json.certificate) : null;
    if (data && isCertificate(data) && rawPublicKey(new crypto.X509Certificate(data).publicKey).equals(publicData)) {
      certificate = data;
    }
  } catch {
    certificate = null;
  }
  if (!certificate) throw new Error("Pairing failed. Create a new invitation on the trusted host.");
  const pkcs8 = privateKey.export({ type: "pkcs8", format: "der" });
  const protection = await safeStorage.isAsyncEncryptionAvailable() ? "safeStorage" : "none";
  const entry = {
    origin: invitation.origin,
    serverCA: invitation.serverCA.toString("base64"),
    serverCertificate: invitation.serverCertificate.toString("base64"),
    certificate: certificate.toString("base64"),
    // Without OS secret storage, the key is protected only by the store file's 0600 mode.
    privateKey: (protection === "safeStorage" ? await safeStorage.encryptStringAsync(pkcs8.toString("base64")) : pkcs8)
      .toString("base64"),
    protection,
    storeID: crypto.randomUUID(),
    route: invitation.route,
  };
  if (full()) throw new Error("Paired host limit reached.");
  writeStore([...readStore().filter((item) => item?.origin !== entry.origin), entry]);
  return url.href;
}

function partitionFor(url) {
  try {
    const record = new URL(url).protocol === "https:" ? stored(url) : null;
    return record ? `persist:paired-${record.storeID}` : null;
  } catch {
    return null;
  }
}

async function startProxy(entry, hostSession, hostname, record) {
  const trust = { ...record, ...(await identity(record)) };
  entry.proxy = await new BridgeProxy(record.origin, () => connectPinned(trust, 15_000)).listen();
  await hostSession.setProxy({ mode: "pac_script", pacScript: entry.proxy.pacURL() });
  // setProxy resolves before the network service applies the PAC; without this, the first lookups
  // on a new persistent partition resolve DIRECT.
  await hostSession.forceReloadProxyConfig();
  verifyPairedHost(hostSession, hostname, entry.proxy);
  return entry.proxy;
}

// Returns false for unpaired hosts. Throws the identity-failure message when the saved pairing or
// device identity is unusable; tunnel identity failures later call onIdentityFailure.
async function configureSession(hostSession, url, { onIdentityFailure } = {}) {
  const parsed = new URL(url);
  if (parsed.protocol !== "https:") return false;
  const hostname = bareHostname(parsed.hostname);
  let record;
  try {
    record = stored(parsed);
  } catch (error) {
    verifyPairedHost(hostSession, hostname, null);
    throw identityFailure(error);
  }
  if (!record) return false;
  let entry = proxies.get(hostSession);
  if (entry?.storeID !== record.storeID) {
    entry?.proxy?.close();
    verifyPairedHost(hostSession, hostname, null);
    entry = { storeID: record.storeID, proxy: null };
    entry.ready = startProxy(entry, hostSession, hostname, record);
    proxies.set(hostSession, entry);
    const started = entry;
    entry.ready.catch(() => {
      started.proxy?.close();
      if (proxies.get(hostSession) === started) proxies.delete(hostSession);
    });
  }
  const proxy = await entry.ready;
  proxy.onIdentityFailure = onIdentityFailure ?? (() => {});
  return true;
}

// Call from app.on("login"); the caller must call event.preventDefault() when this returns true.
function answerProxyLogin(authInfo, callback) {
  const proxy = [...proxies.values()].map((entry) => entry.proxy).find((candidate) => candidate?.answers(authInfo));
  if (!proxy) return false;
  callback(proxy.username, proxy.password);
  return true;
}

function deviceName() {
  const name = os.hostname().replace(/\.local$/i, "").replace(/[\x00-\x1f]/g, " ").trim();
  return Array.from(name || "Xedoc desktop").slice(0, 64).join("");
}

module.exports = {
  parseInvitation, pairedHosts, stored, availableHost, pair, partitionFor, configureSession, answerProxyLogin,
  deviceName,
};
