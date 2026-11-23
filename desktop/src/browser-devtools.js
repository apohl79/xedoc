"use strict";

const domains = new Set([
  "Accessibility", "Animation", "Audits", "CSS", "DOM", "DOMDebugger", "DOMSnapshot",
  "Debugger", "Emulation", "Fetch", "Input", "Inspector", "LayerTree", "Log",
  "Network", "Overlay", "Page", "Performance", "Profiler", "Runtime", "Schema",
]);
const forbidden = new Set([
  "Network.getAllCookies", "Network.getCookies", "Network.setCookie", "Network.setCookies",
  "Network.deleteCookies", "Network.clearBrowserCache", "Network.clearBrowserCookies",
  "Page.setDownloadBehavior", "Page.close",
  "DOM.setFileInputFiles",
]);
const attachOptions = {
  autoAttach: true, waitForDebuggerOnStart: false, flatten: true,
  filter: [{ type: "iframe", exclude: false }, { type: "worker", exclude: false }, { exclude: true }],
};

class BrowserDevtools {
  constructor(contents) {
    this.contents = contents;
    this.debugger = contents.debugger;
    this.sessions = new Map();
    this.events = [];
    this.sequence = 0;
    this.ready = null;
    this.debugger.on("message", (_event, method, params, sessionId) => {
      if (method === "Target.attachedToTarget" && (!sessionId || this.sessions.has(sessionId))) {
        if (["iframe", "worker"].includes(params.targetInfo.type) && this.sessions.size < 32) {
          this.sessions.set(params.sessionId, {
            type: params.targetInfo.type, url: String(params.targetInfo.url).slice(0, 256),
          });
          void this.debugger.sendCommand("Target.setAutoAttach", attachOptions, params.sessionId).catch(() => {});
        }
      }
      if (method === "Target.detachedFromTarget") this.sessions.delete(params.sessionId);
      if (sessionId && !this.sessions.has(sessionId)) return;
      const event = { id: ++this.sequence, method, sessionId: sessionId || null, params };
      if (Buffer.byteLength(JSON.stringify(event)) > 2048) {
        event.params = {
          truncated: true, requestId: String(params.requestId || "").slice(0, 128),
          preview: JSON.stringify(params).slice(0, 200),
        };
      }
      this.events.push(event);
      if (this.events.length > 128) this.events.shift();
    });
    this.debugger.on("detach", () => {
      this.ready = null;
      this.sessions.clear();
      this.events.length = 0;
    });
  }

  async connect() {
    if (!this.ready) {
      this.ready = (async () => {
        if (!this.debugger.isAttached()) this.debugger.attach("1.3");
        await this.debugger.sendCommand("Target.setAutoAttach", attachOptions);
      })().catch((error) => { this.ready = null; throw error; });
    }
    await this.ready;
  }

  async command(input) {
    const method = input.method;
    if (typeof method !== "string" || !/^[A-Za-z]+\.[A-Za-z]+$/.test(method)
        || !domains.has(method.split(".")[0]) || forbidden.has(method)) {
      throw new Error("DevTools method is not panel-scoped or is unsupported.");
    }
    const params = input.params ?? {};
    if (!params || typeof params !== "object" || Array.isArray(params)) throw new Error("DevTools params must be an object.");
    if (method === "Page.navigate") {
      const url = new URL(params.url);
      if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) {
        throw new Error("DevTools navigation requires an HTTP(S) URL without credentials.");
      }
    }
    await this.connect();
    const sessionId = input.sessionId || undefined;
    if (sessionId && !this.sessions.has(sessionId)) throw new Error("DevTools session does not belong to this panel.");
    let result = await this.debugger.sendCommand(method, params, sessionId);
    const path = input.resultPath || [];
    if (!Array.isArray(path) || path.length > 8) throw new Error("Invalid DevTools resultPath.");
    for (const key of path) {
      if (typeof key !== "string" || ["__proto__", "prototype", "constructor"].includes(key)
          || result === null || typeof result !== "object" || !Object.hasOwn(result, key)) {
        throw new Error("DevTools resultPath does not exist.");
      }
      result = result[key];
    }
    if (input.offset !== undefined || input.limit !== undefined) {
      const offset = input.offset ?? 0;
      const limit = input.limit ?? 100;
      if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isSafeInteger(limit) || limit < 1 || limit > 4096) {
        throw new Error("Invalid DevTools result window.");
      }
      if (!Array.isArray(result) && typeof result !== "string") throw new Error("DevTools result window requires an array or string.");
      result = {
        value: result.slice(offset, offset + limit), total: result.length,
        nextOffset: offset + limit < result.length ? offset + limit : null,
      };
    }
    if (Buffer.byteLength(JSON.stringify(result)) > 7000) {
      throw new Error("DevTools result exceeds 7000 bytes; narrow the command or use resultPath with offset/limit.");
    }
    return result;
  }

  async read(input) {
    await this.connect();
    const after = input.after ?? 0;
    const limit = input.limit ?? 4;
    if (!Number.isSafeInteger(after) || after < 0 || !Number.isInteger(limit) || limit < 1 || limit > 8
        || input.method !== undefined && (typeof input.method !== "string" || input.method.length > 128)) {
      throw new Error("Invalid DevTools event cursor/filter.");
    }
    const result = {
      sessions: [...this.sessions].map(([sessionId, info]) => ({ sessionId, type: info.type })),
      events: [], nextCursor: after, oldestId: this.events[0]?.id ?? null, latestId: this.sequence,
    };
    for (const event of this.events) {
      if (event.id <= after) continue;
      if (input.method && event.method !== input.method) { result.nextCursor = event.id; continue; }
      result.events.push(event);
      if (Buffer.byteLength(JSON.stringify(result)) > 7000) { result.events.pop(); break; }
      result.nextCursor = event.id;
      if (result.events.length >= limit) break;
    }
    return result;
  }
}

module.exports = { BrowserDevtools };
