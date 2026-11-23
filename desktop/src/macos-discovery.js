"use strict";

const { spawn } = require("node:child_process");
const { EventEmitter } = require("node:events");
const path = require("node:path");
const { app } = require("electron");

class MacOSDiscovery extends EventEmitter {
  constructor() {
    super();
    this.services = [];
    this.stopped = false;
    const executable = app.isPackaged
      ? path.join(process.resourcesPath, "bin/xedoc-macos-discovery")
      : path.join(__dirname, "../build/macos", process.arch, "xedoc-macos-discovery");
    this.child = spawn(executable, [], { stdio: ["pipe", "pipe", "pipe"] });
    let buffer = "";
    let diagnostic = "";
    this.child.stdin.on("error", () => {});
    this.child.stdout.setEncoding("utf8");
    this.child.stdout.on("data", (chunk) => {
      if (this.stopped) return;
      buffer += chunk;
      let newline;
      while ((newline = buffer.indexOf("\n")) !== -1) {
        const line = buffer.slice(0, newline);
        buffer = buffer.slice(newline + 1);
        if (Buffer.byteLength(line) > 65_536) return this.fail("macOS discovery returned an oversized result.");
        try {
          const message = JSON.parse(line);
          if (message.type === "ready") {
            this.emit("ready");
            continue;
          }
          if (message.type === "waiting") {
            this.emit("waiting", new Error(typeof message.message === "string"
              ? message.message.slice(0, 512) : "Network discovery is waiting for a connection."));
            continue;
          }
          if (message.type === "error") {
            return this.fail(typeof message.message === "string" ? message.message.slice(0, 512) : "macOS discovery failed.");
          }
          if (message.type !== "hosts" || !Array.isArray(message.services) || message.services.length > 64) {
            return this.fail("macOS discovery returned an invalid result.");
          }
          this.services = message.services.map((service) => ({ txt: {
            version: "1", hostname: service?.hostname, origin: service?.origin,
          } }));
          this.emit("change");
        } catch {
          return this.fail("macOS discovery returned an invalid result.");
        }
      }
      if (Buffer.byteLength(buffer) > 65_536) this.fail("macOS discovery returned an oversized result.");
    });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk) => { diagnostic = (diagnostic + chunk).slice(-2_048); });
    this.child.on("error", (error) => this.fail(`Could not start macOS discovery: ${error.message}`));
    this.child.on("close", (code, signal) => {
      clearTimeout(this.killTimer);
      if (!this.stopped) this.fail(`macOS discovery stopped (${signal ?? code}). ${diagnostic}`.trim());
    });
  }

  fail(message) {
    if (this.stopped) return;
    this.stop();
    this.emit("error", new Error(message));
  }

  stop() {
    if (this.stopped) return;
    this.stopped = true;
    this.child.stdin.end();
    this.killTimer = setTimeout(() => this.child.kill("SIGKILL"), 750);
    this.killTimer.unref();
  }
}

module.exports = { MacOSDiscovery };
