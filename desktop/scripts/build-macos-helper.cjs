"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

function build(arch) {
  if (process.platform !== "darwin") return;
  if (!["arm64", "x64"].includes(arch)) throw new Error(`Unsupported macOS helper architecture: ${arch}`);
  const output = path.join(__dirname, "../build/macos", arch, "xedoc-macos-discovery");
  fs.mkdirSync(path.dirname(output), { recursive: true });
  const target = `${arch === "x64" ? "x86_64" : arch}-apple-macosx14.0`;
  const result = spawnSync("/usr/bin/xcrun", [
    "swiftc", "-O", "-target", target, "-framework", "Network",
    path.join(__dirname, "../native/discovery.swift"), "-o", output,
  ], { stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`macOS discovery helper build failed (${result.status})`);
}

module.exports = (context) => {
  if (context.electronPlatformName === "darwin") build(require("electron-builder").Arch[context.arch]);
};

if (require.main === module) build(process.arch);
