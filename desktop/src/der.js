"use strict";

// Minimal DER encoder for the bridge proxy's per-run self-signed certificate.
const crypto = require("node:crypto");

function encode(tag, content) {
  if (content.length < 0x80) return Buffer.concat([Buffer.from([tag, content.length]), content]);
  const length = [];
  for (let rest = content.length; rest > 0; rest = Math.floor(rest / 256)) length.unshift(rest & 0xff);
  return Buffer.concat([Buffer.from([tag, 0x80 | length.length, ...length]), content]);
}

function sequence(...items) {
  return encode(0x30, Buffer.concat(items));
}

function oid(text) {
  const [first, second, ...rest] = text.split(".").map(Number);
  const bytes = [first * 40 + second];
  for (const value of rest) {
    const chunk = [value & 0x7f];
    for (let high = value >>> 7; high > 0; high >>>= 7) chunk.unshift((high & 0x7f) | 0x80);
    bytes.push(...chunk);
  }
  return encode(0x06, Buffer.from(bytes));
}

function utcTime(date) {
  return encode(0x17, Buffer.from(`${date.toISOString().replace(/[-:T]/g, "").slice(2, 14)}Z`));
}

// Returns a P-256 certificate and key; trust comes only from exact DER comparison, never from a CA.
function selfSignedCertificate(commonName) {
  const { privateKey, publicKey } = crypto.generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const algorithm = sequence(oid("1.2.840.10045.4.3.2"));
  const name = sequence(encode(0x31, sequence(oid("2.5.4.3"), encode(0x0c, Buffer.from(commonName)))));
  const serial = crypto.randomBytes(16);
  serial[0] = (serial[0] & 0x7f) | 0x40;
  const now = Date.now();
  const tbs = sequence(
    encode(0xa0, encode(0x02, Buffer.from([2]))),
    encode(0x02, serial),
    algorithm,
    name,
    sequence(utcTime(new Date(now - 3_600_000)), utcTime(new Date(now + 397 * 86_400_000))),
    name,
    publicKey.export({ type: "spki", format: "der" }),
  );
  const signature = crypto.sign("sha256", tbs, privateKey);
  const der = sequence(tbs, algorithm, encode(0x03, Buffer.concat([Buffer.from([0]), signature])));
  return {
    der,
    cert: new crypto.X509Certificate(der).toString(),
    key: privateKey.export({ type: "pkcs8", format: "pem" }),
  };
}

module.exports = { selfSignedCertificate };
