"use strict";

// Small persistent key-value store, the counterpart of UserDefaults in the macOS app.
const fs = require("node:fs");
const path = require("node:path");
const { app } = require("electron");

let cache;

function file() {
  return path.join(app.getPath("userData"), "state.json");
}

function load() {
  if (cache) return cache;
  try {
    const value = JSON.parse(fs.readFileSync(file(), "utf8"));
    cache = value && typeof value === "object" && !Array.isArray(value) ? value : {};
  } catch {
    cache = {};
  }
  return cache;
}

function get(key) {
  return load()[key];
}

function set(key, value) {
  const values = load();
  if (value === undefined) delete values[key];
  else values[key] = value;
  const target = file();
  fs.mkdirSync(path.dirname(target), { recursive: true });
  const temporary = `${target}.${process.pid}.tmp`;
  fs.writeFileSync(temporary, JSON.stringify(values), { mode: 0o600 });
  fs.renameSync(temporary, target);
}

function stringArray(key) {
  const value = get(key);
  return Array.isArray(value) ? value.filter((item) => typeof item === "string") : [];
}

module.exports = { get, set, stringArray };
