"use strict";

const { spawnSync } = require("node:child_process");
const { validatedHostURL } = require("./urls");

function migrateMacOSState(state) {
  if (process.platform !== "darwin" || state.get("macOSPreferencesImported") === true) return;
  const exported = spawnSync("/usr/bin/defaults", ["export", "org.e47.xedoc.desktop", "-"], {
    encoding: "utf8", timeout: 5_000, maxBuffer: 1_048_576,
  });
  // A new installation has no legacy preferences domain.
  if (exported.status !== 0) {
    if (!exported.error && exported.status === 1) state.set("macOSPreferencesImported", true);
    return;
  }
  const read = (key, format) => {
    const result = spawnSync("/usr/bin/plutil", [
      "-extract", key.replaceAll(".", "\\."), format, "-o", "-", "--", "-",
    ], { input: exported.stdout, encoding: "utf8", timeout: 2_000, maxBuffer: 65_536 });
    return result.status === 0 ? result.stdout.trim() : null;
  };
  const saved = read("connectedHosts", "json");
  let hosts = [];
  try {
    const values = JSON.parse(saved);
    if (Array.isArray(values)) hosts = values.slice(0, 64).filter((value) => validatedHostURL(value));
  } catch {}
  if (state.get("connectedHosts") === undefined) {
    state.set("connectedHosts", [...new Set(hosts.map((host) => validatedHostURL(host).href))]);
  }
  if (state.get("connectedLocalHost") === undefined) {
    const local = read("connectedLocalHost", "raw");
    if (local === "true" || local === "false") state.set("connectedLocalHost", local === "true");
  }
  for (const host of ["local", ...hosts]) {
    const key = `hostView:${host === "local" ? host : validatedHostURL(host).href}`;
    if (state.get(key) !== undefined) continue;
    const encoded = read(`hostView:${host}`, "raw");
    if (!encoded || encoded.length > 16_384) continue;
    try {
      const view = JSON.parse(Buffer.from(encoded, "base64").toString("utf8"));
      const keys = ["workspaceId", "projectName", "threadId"];
      if (typeof view?.username !== "string" || !view.username || view.username.length > 64
          || !keys.every((name) => view[name] === null
            || (typeof view[name] === "string" && Buffer.byteLength(view[name]) <= 1024))) continue;
      state.set(key, Object.fromEntries(["username", ...keys].map((name) => [name, view[name]])));
    } catch {}
  }
  // Cookies, device keys, certificates and WebKit data-store identifiers stay with the old app.
  state.set("macOSPreferencesImported", true);
}

module.exports = { migrateMacOSState };
