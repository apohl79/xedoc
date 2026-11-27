"use strict";

const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { randomUUID } = require("node:crypto");

const maxFileBytes = 32 * 1024 * 1024;
const maxRetainedBytes = 512 * 1024 * 1024;
const chunkBytes = 512 * 1024;
const owners = new Map();
const sessions = new WeakSet();
const retained = new Set();

class BrowserDownloads {
  constructor(contents) {
    this.contents = contents;
    this.records = new Map();
    this.error = null;
    owners.set(contents, this);
    if (!sessions.has(contents.session)) {
      sessions.add(contents.session);
      contents.session.on("will-download", (event, item, source) => {
        owners.get(source)?.capture(event, item);
      });
    }
    contents.once("destroyed", () => {
      owners.delete(contents);
      for (const record of this.records.values()) this.remove(record);
      this.records.clear();
    });
  }

  capture(event, item) {
    if (this.records.size >= 12) {
      const oldest = [...this.records.values()].find((record) => record.state !== "progressing");
      if (oldest) this.remove(oldest);
    }
    const retainedBytes = [...retained].reduce((sum, record) => sum + record.receivedBytes, 0);
    if (this.records.size >= 12 || retainedBytes >= maxRetainedBytes) {
      event.preventDefault();
      this.error = "Download cancelled: browser download storage is full. Close unused panels.";
      return;
    }
    const record = {
      downloadId: randomUUID(),
      filename: item.getFilename().replace(/[^a-zA-Z0-9._-]/g, "_").slice(0, 160) || "download",
      mimeType: String(item.getMimeType()).slice(0, 128),
      state: "progressing", receivedBytes: 0, totalBytes: item.getTotalBytes(),
      item, directory: null, path: null,
    };
    this.records.set(record.downloadId, record);
    retained.add(record);
    if (record.totalBytes > maxFileBytes) {
      event.preventDefault();
      record.state = "cancelled";
      record.error = "Download exceeds 32 MiB.";
      record.item = null;
      return;
    }
    try {
      record.directory = fs.mkdtempSync(path.join(os.tmpdir(), "xedoc-browser-"));
      record.path = path.join(record.directory, "download");
      item.setSavePath(record.path);
    } catch {
      event.preventDefault();
      record.state = "cancelled";
      record.error = "Could not create private browser download storage.";
      record.item = null;
      return;
    }
    item.on("updated", () => {
      record.receivedBytes = item.getReceivedBytes();
      record.totalBytes = item.getTotalBytes();
      const bytes = [...retained].reduce((sum, entry) => sum + entry.receivedBytes, 0);
      if (!record.error && (record.receivedBytes > maxFileBytes || record.totalBytes > maxFileBytes || bytes > maxRetainedBytes)) {
        record.error = "Download exceeded browser storage limits (32 MiB per file, 512 MiB total).";
        setImmediate(() => {
          if (record.item === item) item.cancel();
        });
      }
    });
    item.once("done", (_event, state) => {
      record.state = state;
      record.receivedBytes = item.getReceivedBytes();
      record.item = null;
      if (state === "completed") {
        try {
          record.size = fs.statSync(record.path).size;
          if (record.size <= maxFileBytes) return;
          record.error = "Download exceeds 32 MiB.";
        } catch {
          record.error = "Downloaded file is no longer available.";
        }
        record.state = "interrupted";
      }
      if (record.directory) {
        try { fs.rmSync(record.directory, { recursive: true, force: true }); } catch {}
      }
      record.receivedBytes = 0;
    });
  }

  list() {
    return {
      downloads: [...this.records.values()].map((record) => ({
        downloadId: record.downloadId, filename: record.filename, mimeType: record.mimeType,
        state: record.state, receivedBytes: record.receivedBytes, totalBytes: record.totalBytes,
        ...(record.error ? { error: record.error } : {}),
      })),
      ...(this.error ? { error: this.error } : {}),
    };
  }

  read(input) {
    const record = this.records.get(input.downloadId);
    if (!record) throw new Error("Download does not belong to this panel or is no longer retained.");
    if (record.state !== "completed") throw new Error(`Download is ${record.state}; wait for completion before saving.`);
    const offset = input.offset ?? 0;
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > record.size) throw new Error("Invalid download offset.");
    const buffer = Buffer.alloc(Math.min(chunkBytes, record.size - offset));
    const descriptor = fs.openSync(record.path, "r");
    let length;
    try {
      length = fs.readSync(descriptor, buffer, 0, buffer.length, offset);
    } finally {
      fs.closeSync(descriptor);
    }
    return {
      downloadId: record.downloadId, filename: record.filename, mimeType: record.mimeType,
      size: record.size, offset, base64: buffer.subarray(0, length).toString("base64"),
      eof: offset + length === record.size,
    };
  }

  remove(record) {
    record.item?.cancel();
    if (record.directory) {
      try { fs.rmSync(record.directory, { recursive: true, force: true }); } catch {}
    }
    retained.delete(record);
    this.records.delete(record.downloadId);
  }
}

module.exports = { BrowserDownloads };
