"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs/promises");
const path = require("node:path");

const extensionId = "aeblfdkhhhdcdjpifhhbdiojplfjncoa";
const expectedHash =
  "00aaf858dc612e2616d9c3a567c6ecefa91b33415bc7438802894d8c3ea91662";
const downloadUrl = `https://clients2.google.com/service/update2/crx?response=redirect&prodversion=150.0.0.0&acceptformat=crx2,crx3&x=id%3D${extensionId}%26installsource%3Dondemand%26uc`;
const limit = 32 * 1024 * 1024;
const marker = ".xedoc-onepassword-probe";
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest();

function fields(bytes) {
  let offset = 0;
  function varint() {
    let value = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      if (offset >= bytes.length) throw new Error("Truncated CRX protobuf");
      const byte = bytes[offset++];
      value += (byte & 127) * 2 ** shift;
      if (!(byte & 128)) return value;
    }
    throw new Error("Oversized CRX protobuf integer");
  }
  const result = new Map();
  while (offset < bytes.length) {
    const tag = varint();
    if (tag % 8 !== 2 || tag < 8)
      throw new Error("Unexpected CRX protobuf field");
    const length = varint();
    if (length > bytes.length - offset)
      throw new Error("Truncated CRX protobuf field");
    const field = Math.floor(tag / 8);
    result.set(field, [
      ...(result.get(field) || []),
      bytes.subarray(offset, offset + length),
    ]);
    offset += length;
  }
  return result;
}

function verify(crx) {
  if (hash(crx).toString("hex") !== expectedHash) {
    throw new Error(
      "CRX differs from reviewed release 8.12.46.29; refresh requires code review",
    );
  }
  if (
    crx.length < 12 ||
    crx.toString("ascii", 0, 4) !== "Cr24" ||
    crx.readUInt32LE(4) !== 3
  ) {
    throw new Error("Expected CRX3");
  }
  const headerSize = crx.readUInt32LE(8);
  if (headerSize > 1024 * 1024 || headerSize > crx.length - 12)
    throw new Error("Invalid CRX header");
  const header = fields(crx.subarray(12, 12 + headerSize));
  const signedHeaders = header.get(10000);
  if (signedHeaders?.length !== 1) throw new Error("Missing CRX signed header");
  const signedHeader = signedHeaders[0];
  const size = Buffer.alloc(4);
  size.writeUInt32LE(signedHeader.length);
  const zip = crx.subarray(12 + headerSize);
  const signed = Buffer.concat([
    Buffer.from("CRX3 SignedData\0"),
    size,
    signedHeader,
    zip,
  ]);
  for (const proof of header.get(2) || []) {
    const values = fields(proof);
    const key = values.get(1)?.[0];
    const signature = values.get(2)?.[0];
    if (!key || !signature) continue;
    const idBytes = hash(key).subarray(0, 16);
    const id = idBytes
      .toString("hex")
      .replace(/[0-9a-f]/g, (digit) =>
        String.fromCharCode(97 + parseInt(digit, 16)),
      );
    if (id !== extensionId) continue;
    const signedId = fields(signedHeader).get(1);
    if (signedId?.length !== 1 || !signedId[0].equals(idBytes))
      throw new Error("CRX identity mismatch");
    if (
      !crypto.verify(
        "RSA-SHA256",
        signed,
        { key, format: "der", type: "spki" },
        signature,
      )
    ) {
      throw new Error("Invalid official developer signature");
    }
    return { zip, key: key.toString("base64") };
  }
  throw new Error("Official 1Password developer proof not found");
}

async function prepare({
  cache = path.join(__dirname, "../.cache/onepassword"),
  destination = path.join(__dirname, "../build/onepassword"),
} = {}) {
  await fs.mkdir(cache, { recursive: true });
  if ((await fs.lstat(cache)).isSymbolicLink())
    throw new Error("Refusing symlink cache");
  const crxPath = path.join(cache, "1password.crx");
  let crx;
  try {
    const stat = await fs.lstat(crxPath);
    if (!stat.isFile() || stat.size > limit)
      throw new Error("Invalid cached CRX");
    crx = await fs.readFile(crxPath);
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    const response = await fetch(downloadUrl, {
      signal: AbortSignal.timeout(60_000),
    });
    if (!response.ok || !response.body)
      throw new Error(`CRX download failed: ${response.status}`);
    const chunks = [];
    let length = 0;
    for await (const chunk of response.body) {
      length += chunk.length;
      if (length > limit) throw new Error("CRX exceeds 32 MiB download limit");
      chunks.push(chunk);
    }
    crx = Buffer.concat(chunks);
    verify(crx);
    await fs.writeFile(crxPath, crx, { flag: "wx" });
  }
  const { zip, key } = verify(crx);
  await fs.mkdir(path.dirname(destination), { recursive: true });
  try {
    const stat = await fs.lstat(destination);
    if (
      !stat.isDirectory() ||
      (await fs.readFile(path.join(destination, marker), "utf8")) !==
        expectedHash
    ) {
      throw new Error(
        "Refusing to replace unrelated extension directory; move it aside manually",
      );
    }
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
    // A directory without our marker is not generated output.
    try {
      await fs.lstat(destination);
      throw new Error("Refusing extension directory without generation marker");
    } catch (missing) {
      if (missing.code !== "ENOENT") throw missing;
    }
  }
  const staging = await fs.mkdtemp(path.join(cache, "extract-"));
  try {
    const zipPath = path.join(staging, "extension.zip");
    const unpacked = path.join(staging, "extension");
    await fs.writeFile(zipPath, zip);
    const { default: extract } = await import(
      require.resolve("@electron-internal/extract-zip")
    );
    await extract(zipPath, { dir: unpacked });
    const manifestPath = path.join(unpacked, "manifest.json");
    const manifest = JSON.parse(await fs.readFile(manifestPath, "utf8"));
    manifest.key = key;
    await fs.writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
    await fs.writeFile(path.join(unpacked, marker), expectedHash);
    await fs.rm(destination, { recursive: true, force: true });
    await fs.rename(unpacked, destination);
    console.log(
      `Prepared official 1Password ${manifest.version} (${extensionId}); developer signature verified.`,
    );
  } finally {
    await fs.rm(staging, { recursive: true, force: true });
  }
}

module.exports = { prepare };

if (require.main === module) {
  prepare().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
