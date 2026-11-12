"use strict";

window.XedocPluginPanels = class {
  constructor(api, context, run) {
    this.api = api;
    this.context = context;
    this.run = run;
    this.entries = new Map();
    this.selected = new Map();
    this.fullscreen = new Set();
    this.actions = [];
    this.key = null;
    this.refreshing = new Set();
    this.opening = new Map();
    this.browserPanels = new Map();
    this.container = document.getElementById("plugin-panels");
    this.tabs = document.getElementById("plugin-panel-tabs");
    this.frames = document.getElementById("plugin-panel-frames");
    this.divider = document.getElementById("plugin-panel-divider");
    const layout = document.getElementById("session-layout");
    const resize = (percentage) => {
      const width = Math.min(80, Math.max(20, percentage));
      layout.style.setProperty("--plugin-panel-width", `${width}%`);
      this.divider.setAttribute("aria-valuenow", String(Math.round(width)));
    };
    this.divider.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      this.divider.setPointerCapture(event.pointerId);
      layout.classList.add("panel-resizing");
    });
    this.divider.addEventListener("pointermove", (event) => {
      if (!this.divider.hasPointerCapture(event.pointerId)) return;
      const bounds = layout.getBoundingClientRect();
      resize((bounds.right - event.clientX) / bounds.width * 100);
    });
    this.divider.addEventListener("pointerup", (event) => {
      if (this.divider.hasPointerCapture(event.pointerId)) {
        this.divider.releasePointerCapture(event.pointerId);
      }
    });
    this.divider.addEventListener("lostpointercapture", () => {
      layout.classList.remove("panel-resizing");
    });
    this.divider.addEventListener("keydown", (event) => {
      const width = Number(this.divider.getAttribute("aria-valuenow"));
      const next = { ArrowLeft: width + 5, ArrowRight: width - 5, Home: 20, End: 80 }[event.key];
      if (next === undefined) return;
      event.preventDefault();
      resize(next);
    });
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
      if (event.data?.type === "xedoc-plugin-check") {
        this.checkTarget(current, event.data);
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

  async checkTarget(current, data) {
    if (typeof data.requestId !== "string" || data.requestId.length > 128
      || typeof data.target !== "string" || !data.target || data.target.length > 4096) return;
    const key = this.contextKey(current);
    let result;
    try {
      result = await this.api("plugin/actions", { ...this.params(current), target: data.target });
    } catch (error) {
      result = { actions: [], error: error.message };
    }
    if (key !== this.contextKey(this.context())) return;
    current.frame.contentWindow.postMessage({
      type: "xedoc-plugin-checked", requestId: data.requestId, ...result,
    }, location.origin);
  }

  async refresh() {
    const current = this.context();
    const key = this.contextKey(current);
    if (!key || this.refreshing.has(key) || [...this.opening.values()].includes(key)) return;
    this.refreshing.add(key);
    try {
      const result = await this.api(`plugin/panels?${new URLSearchParams(this.params(current))}`);
      if (key !== this.contextKey(this.context()) || [...this.opening.values()].includes(key)) return;
      this.actions = Array.isArray(result.actions) ? result.actions : [];
      const panels = Array.isArray(result.panels) ? result.panels : [];
      const ids = new Set(panels.map((panel) => panel.panelId));
      for (const [id, owner] of this.browserPanels) {
        if (owner === key && !ids.has(id)) this.browserPanels.delete(id);
      }
      let changed = false;
      for (const [id, entry] of this.entries) {
        if (entry.key === key && !ids.has(id)) {
          entry.frame.remove();
          this.entries.delete(id);
          changed = true;
        }
      }
      for (const panel of panels) {
        if (!this.browserPanels.has(panel.panelId)) changed = this.add(panel, key) || changed;
      }
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
    frame.setAttribute("sandbox", "allow-scripts allow-forms allow-downloads allow-top-navigation-to-custom-protocols");
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
    if (!visible.length) {
      this.fullscreen.delete(this.key);
    }
    const fullscreen = this.fullscreen.has(this.key);
    document.getElementById("session-layout").classList.toggle("panel-fullscreen", fullscreen);
    for (const entry of this.entries.values()) {
      entry.frame.hidden = entry.key !== this.key || entry.panelId !== selected;
    }
    this.tabs.replaceChildren();
    for (const entry of visible) {
      const tab = document.createElement("div");
      tab.className = "session-tab plugin-panel-tab";
      tab.setAttribute("role", "tab");
      tab.setAttribute("aria-selected", String(entry.panelId === selected));
      tab.tabIndex = 0;
      const select = document.createElement("span");
      select.textContent = entry.title || "Plugin panel";
      select.title = entry.target || select.textContent;
      const activate = () => {
        this.selected.set(this.key, entry.panelId);
        this.render();
        this.tabs.querySelector('[aria-selected="true"]')?.focus();
      };
      tab.addEventListener("click", activate);
      tab.addEventListener("keydown", (event) => {
        if (event.target === tab && (event.key === "Enter" || event.key === " ")) {
          event.preventDefault();
          activate();
        }
      });
      const close = document.createElement("button");
      close.type = "button";
      close.className = "close-tab";
      close.textContent = "×";
      close.setAttribute("aria-label", `Close ${entry.title || "plugin panel"}`);
      close.addEventListener("click", (event) => {
        event.stopPropagation();
        this.run(async () => {
          const current = this.context();
          if (this.contextKey(current) !== entry.key) return;
          close.disabled = true;
          try {
            await this.api("plugin/close", { ...this.params(current), panelId: entry.panelId });
            entry.frame.remove();
            this.entries.delete(entry.panelId);
            this.render();
            this.tabs.querySelector('[role="tab"]')?.focus();
          } finally {
            close.disabled = false;
          }
        });
      });
      const browser = document.createElement("a");
      browser.href = entry.url;
      browser.target = "_blank";
      browser.rel = "noopener noreferrer";
      browser.className = "plugin-panel-open";
      browser.textContent = "↗";
      browser.setAttribute("aria-label", `Open ${entry.title || "plugin panel"} in browser`);
      browser.addEventListener("click", (event) => event.stopPropagation());
      const expand = document.createElement("button");
      expand.type = "button";
      expand.className = "plugin-panel-expand";
      const expanded = fullscreen && entry.panelId === selected;
      expand.title = expanded ? "Restore split view" : "Expand panel";
      expand.setAttribute("aria-label", expand.title);
      expand.setAttribute("aria-pressed", String(expanded));
      expand.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"
        aria-hidden="true" focusable="false"><path d="${expanded
          ? "M8 3v5H3m13-5v5h5M8 21v-5H3m13 5v-5h5"
          : "M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5"}"/></svg>`;
      expand.addEventListener("click", (event) => {
        event.stopPropagation();
        this.selected.set(this.key, entry.panelId);
        if (expanded) this.fullscreen.delete(this.key);
        else this.fullscreen.add(this.key);
        this.render();
        this.tabs.querySelector('[aria-selected="true"] .plugin-panel-expand')?.focus();
      });
      tab.append(select, expand, browser, close);
      this.tabs.append(tab);
    }
    this.container.hidden = !visible.length;
    this.divider.hidden = !visible.length || fullscreen;
  }

  async open(current, data) {
    const action = this.actions.find((item) => item.pluginId === data.pluginId
      && item.panelId === data.panelId && item.handlerId === data.handlerId);
    if (!action || typeof data.target !== "string") return;
    const key = this.contextKey(current);
    const launch = JSON.stringify([key, action.pluginId, action.panelId, action.handlerId, data.target]);
    if (this.opening.has(launch)) return;
    if (data.destination !== undefined && !["xedoc", "browser"].includes(data.destination)) return;
    this.opening.set(launch, key);
    try {
      const panel = await this.api("plugin/open", {
        ...this.params(current), pluginId: action.pluginId, panelId: action.panelId,
        handlerId: action.handlerId, target: data.target,
      });
      if (data.destination === "browser") {
        if (!this.entries.has(panel.panelId)) this.browserPanels.set(panel.panelId, key);
      } else {
        this.browserPanels.delete(panel.panelId);
        this.add(panel, key);
        this.selected.set(key, panel.panelId);
        this.render();
      }
      current.frame.contentWindow.postMessage({
        type: "xedoc-plugin-result", requestId: data.requestId, url: panel.url,
      }, location.origin);
    } catch (error) {
      current.frame.contentWindow.postMessage({
        type: "xedoc-plugin-result", requestId: data.requestId, error: error.message,
      }, location.origin);
      throw error;
    } finally {
      this.opening.delete(launch);
    }
  }

  removeSession(threadId) {
    for (const [id, key] of this.browserPanels) {
      if (JSON.parse(key)[3] === threadId) this.browserPanels.delete(id);
    }
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
    this.fullscreen.clear();
    this.browserPanels.clear();
    this.frames.replaceChildren();
    this.key = null;
    this.actions = [];
    this.render();
  }
};
