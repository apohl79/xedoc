"use strict";

window.XedocPluginPanels = class {
  constructor(api, context, run) {
    this.api = api;
    this.context = context;
    this.run = run;
    this.entries = new Map();
    this.selected = new Map();
    this.actions = [];
    this.key = null;
    this.refreshing = new Set();
    this.container = document.getElementById("plugin-panels");
    this.tabs = document.getElementById("plugin-panel-tabs");
    this.frames = document.getElementById("plugin-panel-frames");
    new MutationObserver(() => this.sync()).observe(document.getElementById("host-view"), {
      subtree: true, attributes: true, attributeFilter: ["hidden"],
    });
    window.addEventListener("message", (event) => {
      const current = this.context();
      if (!current || event.origin !== location.origin
        || event.source !== current.frame.contentWindow) return;
      if (event.data?.type === "xedoc-plugin-ready") this.publish(current);
      if (event.data?.type === "xedoc-plugin-open") {
        this.run(() => this.open(current, event.data));
      }
    });
    setInterval(() => this.refresh(), 2000);
  }

  contextKey(context) {
    return context ? JSON.stringify([
      context.generation, context.workspaceId, context.projectName, context.threadId,
    ]) : null;
  }

  params(context) {
    return {
      workspaceId: context.workspaceId,
      projectName: context.projectName,
      threadId: context.threadId,
    };
  }

  sync() {
    const key = this.contextKey(this.context());
    if (key === this.key) return;
    this.key = key;
    this.actions = [];
    this.render();
    this.publish(this.context());
    this.refresh();
  }

  publish(context) {
    context?.frame.contentWindow.postMessage({
      type: "xedoc-plugin-actions", actions: this.actions,
    }, location.origin);
  }

  async refresh() {
    const current = this.context();
    const key = this.contextKey(current);
    if (!key || this.refreshing.has(key)) return;
    this.refreshing.add(key);
    try {
      const result = await this.api(`plugin/panels?${new URLSearchParams(this.params(current))}`);
      if (key !== this.contextKey(this.context())) return;
      this.actions = Array.isArray(result.actions) ? result.actions : [];
      const panels = Array.isArray(result.panels) ? result.panels : [];
      const ids = new Set(panels.map((panel) => panel.panelId));
      let changed = false;
      for (const [id, entry] of this.entries) {
        if (entry.key === key && !ids.has(id)) {
          entry.frame.remove();
          this.entries.delete(id);
          changed = true;
        }
      }
      for (const panel of panels) changed = this.add(panel, key) || changed;
      if (changed) this.render();
      this.publish(current);
    } catch {
      // Explicit open/close actions report errors; discovery retries without interrupting work.
    } finally {
      this.refreshing.delete(key);
    }
  }

  add(panel, key) {
    if (typeof panel.panelId !== "string" || typeof panel.url !== "string"
      || !panel.url.startsWith("/plugin-ui/") || panel.url.startsWith("//")) return false;
    if (this.entries.has(panel.panelId)) return false;
    const frame = document.createElement("iframe");
    frame.className = "plugin-panel-frame";
    frame.title = panel.title || "Plugin panel";
    frame.setAttribute("sandbox", "allow-scripts allow-forms allow-downloads");
    frame.src = panel.url;
    frame.hidden = true;
    this.entries.set(panel.panelId, { ...panel, key, frame });
    this.frames.append(frame);
    this.selected.set(key, panel.panelId);
    return true;
  }

  render() {
    const visible = [...this.entries.values()].filter((entry) => entry.key === this.key);
    if (!visible.some((entry) => entry.panelId === this.selected.get(this.key))) {
      this.selected.set(this.key, visible[0]?.panelId);
    }
    const selected = this.selected.get(this.key);
    for (const entry of this.entries.values()) {
      entry.frame.hidden = entry.key !== this.key || entry.panelId !== selected;
    }
    this.tabs.replaceChildren();
    for (const entry of visible) {
      const tab = document.createElement("div");
      tab.className = "plugin-panel-tab";
      const select = document.createElement("button");
      select.type = "button";
      select.textContent = entry.title || "Plugin panel";
      select.title = entry.target || select.textContent;
      select.setAttribute("aria-pressed", String(entry.panelId === selected));
      select.addEventListener("click", () => {
        this.selected.set(this.key, entry.panelId);
        this.render();
        this.tabs.querySelector('[aria-pressed="true"]')?.focus();
      });
      const close = document.createElement("button");
      close.type = "button";
      close.textContent = "×";
      close.setAttribute("aria-label", `Close ${entry.title || "plugin panel"}`);
      close.addEventListener("click", () => this.run(async () => {
        const current = this.context();
        if (this.contextKey(current) !== entry.key) return;
        close.disabled = true;
        try {
          await this.api("plugin/close", { ...this.params(current), panelId: entry.panelId });
          entry.frame.remove();
          this.entries.delete(entry.panelId);
          this.render();
          this.tabs.querySelector("button")?.focus();
        } finally {
          close.disabled = false;
        }
      }));
      const browser = document.createElement("a");
      browser.href = entry.url;
      browser.target = "_blank";
      browser.rel = "noopener noreferrer";
      browser.className = "plugin-panel-open";
      browser.textContent = "↗";
      browser.setAttribute("aria-label", `Open ${entry.title || "plugin panel"} in browser`);
      tab.append(select, browser, close);
      this.tabs.append(tab);
    }
    this.container.hidden = !visible.length;
  }

  async open(current, data) {
    const action = this.actions.find((item) => item.pluginId === data.pluginId
      && item.panelId === data.panelId && item.handlerId === data.handlerId);
    if (!action || typeof data.target !== "string") return;
    const key = this.contextKey(current);
    try {
      const panel = await this.api("plugin/open", {
        ...this.params(current), pluginId: action.pluginId, panelId: action.panelId,
        handlerId: action.handlerId, target: data.target,
      });
      if (key !== this.contextKey(this.context())) return;
      this.add(panel, key);
      this.selected.set(key, panel.panelId);
      this.render();
      current.frame.contentWindow.postMessage({
        type: "xedoc-plugin-result", requestId: data.requestId,
      }, location.origin);
    } catch (error) {
      if (key !== this.contextKey(this.context())) return;
      current.frame.contentWindow.postMessage({
        type: "xedoc-plugin-result", requestId: data.requestId, error: error.message,
      }, location.origin);
      throw error;
    }
  }

  removeSession(threadId) {
    for (const [id, entry] of this.entries) {
      if (JSON.parse(entry.key)[3] !== threadId) continue;
      entry.frame.remove();
      this.entries.delete(id);
    }
    this.sync();
    this.render();
  }

  clear() {
    this.entries.clear();
    this.selected.clear();
    this.frames.replaceChildren();
    this.key = null;
    this.actions = [];
    this.render();
  }
};
