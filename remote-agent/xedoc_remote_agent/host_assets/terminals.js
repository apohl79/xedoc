"use strict";
// Colors and font mirror the City Lights theme in cmux's Ghostty config.
const XEDOC_TERMINAL_OPTIONS = {
  fontFamily: '"IosevkaTerm Nerd Font Mono", "Symbols Nerd Font", "Apple Symbols", Menlo, monospace',
  fontSize: 13,
  fontWeight: "500",
  cursorStyle: "block",
  cursorBlink: false,
  scrollback: 5000,
  theme: {
    background: "#1A2128",
    foreground: "#A0B3C5",
    cursor: "#5EC4FF",
    cursorAccent: "#1A2128",
    selectionBackground: "#718CA1",
    selectionForeground: "#1A2128",
    black: "#10151C",
    red: "#D95468",
    green: "#8BD49C",
    yellow: "#EBBF83",
    blue: "#539AFC",
    magenta: "#E27E8D",
    cyan: "#008B94",
    white: "#A0B3C5",
    brightBlack: "#56697A",
    brightRed: "#D98E48",
    brightGreen: "#33CED8",
    brightYellow: "#EBBF83",
    brightBlue: "#5EC4FF",
    brightMagenta: "#B62D65",
    brightCyan: "#70E1E8",
    brightWhite: "#9CAABB",
  },
};

window.XedocHostTerminals = class {
  constructor(api, host) {
    this.api = api;
    this.host = host;
    this.entries = new Map();
    this.active = null;
    this.generation = 0;
    this.encoder = new TextEncoder();
    // WebKit lets a focus() inside a hidden same-origin session frame take focus from the terminal.
    window.addEventListener("blur", () => window.setTimeout(() => {
      const entry = this.entries.get(this.active);
      if (entry?.term && document.activeElement?.matches(".session-frame[hidden]")) entry.term.focus();
    }));
  }

  async create(workspaceId, projectName) {
    const generation = this.generation;
    const pending = Symbol("terminal");
    const entry = { id: pending, workspaceId, projectName, title: "Terminal", offset: 0, retry: 0 };
    this.entries.set(pending, entry);
    this.show(pending);
    let created;
    try {
      created = await this.api("terminal/new", {
        workspaceId, projectName, cols: entry.term.cols, rows: entry.term.rows,
      });
    } catch (error) {
      this.remove(entry);
      throw error;
    }
    if (generation !== this.generation || this.entries.get(pending) !== entry) {
      void this.api("terminal/close", { terminalId: created.terminalId }).catch(() => {});
      return;
    }
    this.entries.delete(pending);
    entry.id = created.terminalId;
    entry.title = created.title || entry.title;
    this.entries.set(entry.id, entry);
    this.tab(entry);
    const current = this.host.current();
    if (current.workspaceId === workspaceId && current.projectName === projectName) {
      document.getElementById("sessions").append(entry.tab);
      entry.tab.scrollIntoView({ block: "nearest", inline: "end" });
    }
    if (this.active === pending) this.show(entry.id);
  }

  async sync() {
    const generation = this.generation;
    const { terminals } = await this.api("terminal/list");
    if (generation !== this.generation) return;
    const live = new Set(terminals.map((item) => item.terminalId));
    for (const entry of [...this.entries.values()]) {
      if (typeof entry.id === "string" && !live.has(entry.id)) this.dispose(entry);
    }
    for (const item of terminals) {
      if (this.entries.has(item.terminalId)) continue;
      const entry = {
        id: item.terminalId, workspaceId: item.workspaceId, projectName: item.projectName,
        title: item.title || "Terminal", offset: 0, retry: 0,
      };
      this.entries.set(entry.id, entry);
      this.tab(entry);
    }
  }

  tabsFor(workspaceId, projectName) {
    return [...this.entries.values()]
      .filter((entry) => entry.tab && entry.workspaceId === workspaceId
        && entry.projectName === projectName)
      .map((entry) => entry.tab);
  }

  show(id) {
    const entry = this.entries.get(id);
    if (!entry) return;
    this.host.reveal();
    this.active = id;
    for (const other of this.entries.values()) {
      if (other.frame) other.frame.hidden = other !== entry;
      other.tab?.setAttribute("aria-selected", String(other === entry));
    }
    this.mount(entry);
    entry.fit.fit();
    if (typeof id === "string" && !entry.socket && !entry.ended) this.connect(entry);
    entry.term.focus();
  }

  hideAll() {
    this.active = null;
    for (const entry of this.entries.values()) {
      if (entry.frame) entry.frame.hidden = true;
      entry.tab?.setAttribute("aria-selected", "false");
    }
  }

  hideOutside(workspaceId, projectName) {
    const entry = this.entries.get(this.active);
    if (entry && (entry.workspaceId !== workspaceId || entry.projectName !== projectName)) {
      this.hideAll();
    }
  }

  async close(id) {
    const entry = this.entries.get(id);
    if (!entry) return;
    await this.api("terminal/close", { terminalId: id });
    this.remove(entry);
  }

  clear() {
    this.generation++;
    for (const entry of [...this.entries.values()]) this.dispose(entry);
    this.active = null;
  }

  tab(entry) {
    const element = document.createElement("div");
    element.className = "session-tab terminal-tab";
    element.dataset.terminalId = entry.id;
    element.setAttribute("role", "tab");
    element.setAttribute("aria-selected", String(this.active === entry.id));
    element.tabIndex = 0;
    element.title = entry.title;
    const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    icon.setAttribute("viewBox", "0 0 24 24");
    icon.setAttribute("aria-hidden", "true");
    icon.setAttribute("focusable", "false");
    for (const pathData of ["m4 17 6-6-6-6", "M12 19h8"]) {
      const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      path.setAttribute("d", pathData);
      icon.append(path);
    }
    const label = document.createElement("span");
    label.className = "session-title";
    label.textContent = entry.title;
    const close = document.createElement("button");
    close.type = "button";
    close.className = "close-tab";
    close.textContent = "×";
    close.setAttribute("aria-label", `Close ${entry.title}`);
    element.append(icon, label, close);
    element.addEventListener("click", () => this.host.run(async () => this.show(entry.id)));
    element.addEventListener("keydown", (event) => {
      if (event.target === element && (event.key === "Enter" || event.key === " ")) {
        event.preventDefault();
        this.host.run(async () => this.show(entry.id));
      }
    });
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      this.host.run(() => this.close(entry.id));
    });
    entry.tab = element;
    entry.label = label;
  }

  mount(entry) {
    if (entry.frame) return;
    entry.frame = document.createElement("div");
    entry.frame.className = "terminal-frame";
    document.getElementById("session-frames").append(entry.frame);
    entry.term = new window.Terminal(XEDOC_TERMINAL_OPTIONS);
    entry.fit = new window.FitAddon.FitAddon();
    entry.term.loadAddon(entry.fit);
    entry.term.open(entry.frame);
    entry.term.onData((data) => this.send(entry, this.encoder.encode(data)));
    entry.term.onBinary((data) => {
      this.send(entry, Uint8Array.from(data, (character) => character.charCodeAt(0)));
    });
    entry.term.onResize(() => this.sendSize(entry));
    entry.term.onTitleChange((title) => {
      const value = title.trim() || entry.title;
      if (!entry.label) return;
      entry.label.textContent = value;
      entry.tab.title = value;
    });
    entry.observer = new ResizeObserver(() => {
      if (!entry.frame.hidden && entry.frame.clientWidth) entry.fit.fit();
    });
    entry.observer.observe(entry.frame);
  }

  connect(entry) {
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const socket = new WebSocket(`${scheme}://${location.host}/api/terminal/socket?${
      new URLSearchParams({ terminalId: entry.id, offset: String(entry.offset) })}`);
    socket.binaryType = "arraybuffer";
    entry.socket = socket;
    socket.addEventListener("open", () => {
      entry.retry = 0;
      this.sendSize(entry);
    });
    socket.addEventListener("message", (event) => {
      if (typeof event.data !== "string") {
        const bytes = new Uint8Array(event.data);
        entry.offset += bytes.length;
        entry.term.write(bytes);
        return;
      }
      const message = JSON.parse(event.data);
      if (message.type === "reset") {
        entry.term.reset();
        entry.offset = message.offset;
      } else if (message.type === "exit") {
        if (message.exitCode === 0) {
          entry.ended = true;
          this.remove(entry);
        } else {
          this.end(entry, message.exitCode == null
            ? "Process exited" : `Process exited with code ${message.exitCode}`);
        }
      }
    });
    socket.addEventListener("close", (event) => {
      if (entry.socket !== socket) return;
      entry.socket = null;
      if (entry.ended || this.entries.get(entry.id) !== entry) return;
      if (event.code === 4404 || event.code === 1008) {
        this.end(entry, "Terminal is no longer available");
        return;
      }
      entry.timer = window.setTimeout(() => {
        entry.timer = null;
        if (this.entries.get(entry.id) === entry && !entry.socket && !entry.ended) {
          this.connect(entry);
        }
      }, Math.min(10_000, 500 * 2 ** entry.retry++));
    });
  }

  end(entry, message) {
    entry.ended = true;
    entry.term?.write(`\r\n\x1b[2m[${message}]\x1b[0m\r\n`);
  }

  send(entry, bytes) {
    if (entry.socket?.readyState !== WebSocket.OPEN) return;
    for (let start = 0; start < bytes.length; start += 65536) {
      entry.socket.send(bytes.subarray(start, start + 65536));
    }
  }

  sendSize(entry) {
    if (entry.socket?.readyState !== WebSocket.OPEN) return;
    entry.socket.send(JSON.stringify({ type: "resize", cols: entry.term.cols, rows: entry.term.rows }));
  }

  remove(entry) {
    const visible = this.active === entry.id;
    this.dispose(entry);
    if (visible) this.host.closed();
  }

  dispose(entry) {
    if (this.entries.get(entry.id) === entry) this.entries.delete(entry.id);
    if (this.active === entry.id) this.active = null;
    window.clearTimeout(entry.timer);
    const socket = entry.socket;
    entry.socket = null;
    socket?.close();
    entry.observer?.disconnect();
    entry.term?.dispose();
    entry.frame?.remove();
    entry.tab?.remove();
  }
};
