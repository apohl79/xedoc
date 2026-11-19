"use strict";

// Chromium can only present client certificates from the OS store, so each paired host's session
// tunnels through this loopback CONNECT proxy. The proxy terminates the webview's TLS with a per-run
// certificate and opens its own pinned mTLS connection to the host, then forwards bytes.
const crypto = require("node:crypto");
const http = require("node:http");
const net = require("node:net");
const tls = require("node:tls");
const { selfSignedCertificate } = require("./der");

const IDENTITY_FAILURE =
  "Host identity verification failed. Obtain a new pairing invitation directly from your trusted host.";

function identityFailure(cause) {
  const error = new Error(IDENTITY_FAILURE, { cause });
  error.identityFailure = true;
  return error;
}

function timeoutError() {
  return Object.assign(new Error("The request timed out."), { code: "ETIMEDOUT" });
}

function bareHostname(hostname) {
  return hostname.replace(/^\[(.*)\]$/, "$1").toLowerCase();
}

// The pinned CA is the only trust anchor, the hostname must match, and the leaf must equal the pin.
function connectPinned(trust, timeout) {
  const url = new URL(trust.origin);
  const host = bareHostname(url.hostname);
  return new Promise((resolve, reject) => {
    let socket;
    try {
      socket = tls.connect({
        host,
        port: Number(url.port || 443),
        servername: net.isIP(host) ? undefined : host,
        ca: [new crypto.X509Certificate(trust.serverCA).toString()],
        cert: trust.cert,
        key: trust.key,
        ALPNProtocols: ["http/1.1"],
        rejectUnauthorized: false,
      });
    } catch (error) {
      reject(identityFailure(error));
      return;
    }
    const timer = setTimeout(() => socket.destroy(timeoutError()), timeout);
    socket.on("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    socket.once("secureConnect", () => {
      clearTimeout(timer);
      if (!socket.authorized || !socket.getPeerCertificate().raw?.equals(trust.serverCertificate)) {
        socket.destroy();
        reject(identityFailure(socket.authorizationError));
        return;
      }
      resolve(socket);
    });
  });
}

class BridgeProxy {
  constructor(origin, connect) {
    const url = new URL(origin);
    this.authority = url.host;
    this.target = `${url.hostname}:${url.port || 443}`;
    this.connect = connect;
    this.onIdentityFailure = () => {};
    this.realm = `Xedoc paired host ${crypto.randomUUID()}`;
    this.username = "xedoc";
    this.password = crypto.randomBytes(32).toString("hex");
    this.expected = Buffer.from(`Basic ${Buffer.from(`${this.username}:${this.password}`).toString("base64")}`);
    this.certificate = selfSignedCertificate("Xedoc paired host proxy");
    this.context = tls.createSecureContext({ cert: this.certificate.cert, key: this.certificate.key });
    this.server = http.createServer((_request, response) => response.writeHead(405, { Connection: "close" }).end());
    this.server.maxConnections = 64;
    this.server.on("connect", (request, socket, head) => this.tunnel(request, socket, head));
    this.server.on("clientError", (_error, socket) => socket.destroy());
  }

  listen() {
    return new Promise((resolve, reject) => {
      this.server.once("error", reject);
      this.server.listen(0, "127.0.0.1", () => {
        this.server.off("error", reject);
        this.port = this.server.address().port;
        resolve(this);
      });
    });
  }

  close() {
    this.server.close();
  }

  tunnel(request, socket, head) {
    socket.on("error", () => socket.destroy());
    const supplied = Buffer.from(request.headers["proxy-authorization"] ?? "");
    if (supplied.length !== this.expected.length || !crypto.timingSafeEqual(supplied, this.expected)) {
      socket.end(`HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm="${this.realm}"\r\n`
        + "Content-Length: 0\r\nConnection: close\r\n\r\n");
      return;
    }
    const reply = (status) => socket.end(`HTTP/1.1 ${status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`);
    if (request.url.toLowerCase() !== this.target) {
      reply("403 Forbidden");
      return;
    }
    // Connect upstream first so a pin or identity failure surfaces as a tunnel failure.
    this.connect().then((upstream) => {
      if (socket.destroyed) {
        upstream.destroy();
        return;
      }
      socket.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      if (head.length) socket.unshift(head);
      const client = new tls.TLSSocket(socket, {
        isServer: true, secureContext: this.context, ALPNProtocols: ["http/1.1"],
      });
      const close = () => {
        client.destroy();
        upstream.destroy();
      };
      for (const side of [client, upstream]) side.on("error", close).on("close", close);
      client.pipe(upstream).pipe(client);
    }, (error) => {
      if (error.identityFailure) this.onIdentityFailure();
      reply("502 Bad Gateway");
    });
  }

  answers(authInfo) {
    return Boolean(authInfo?.isProxy) && authInfo.host === "127.0.0.1" && authInfo.port === this.port
      && authInfo.realm === this.realm;
  }

  // Only the paired origin goes through the proxy; everything else connects directly.
  pacURL() {
    const prefixes = JSON.stringify([`https://${this.authority}/`, `wss://${this.authority}/`]);
    const script = `function FindProxyForURL(url, host) {
  var prefixes = ${prefixes};
  for (var index = 0; index < prefixes.length; index++) {
    if (url.toLowerCase().indexOf(prefixes[index]) === 0) return "PROXY 127.0.0.1:${this.port}";
  }
  return "DIRECT";
}`;
    return `data:application/x-ns-proxy-autoconfig;base64,${Buffer.from(script).toString("base64")}`;
  }

  presents(certificate) {
    const body = /-----BEGIN CERTIFICATE-----([^-]+)-----END CERTIFICATE-----/.exec(certificate?.data ?? "");
    return Boolean(body) && Buffer.from(body[1].replace(/\s+/g, ""), "base64").equals(this.certificate.der);
  }
}

// For the paired hostname, trust only this proxy's certificate (none without a proxy); other hosts
// keep Chromium's verification.
function verifyPairedHost(hostSession, hostname, proxy) {
  hostSession.setCertificateVerifyProc((request, callback) => {
    if (bareHostname(request.hostname) !== hostname) callback(-3);
    else callback(proxy?.presents(request.certificate) ? 0 : -2);
  });
}

module.exports = { BridgeProxy, connectPinned, identityFailure, timeoutError, bareHostname, verifyPairedHost };
