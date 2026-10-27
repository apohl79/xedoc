"use strict";
window.XedocHostUpdates = class {
  constructor(api) {
    this.api = api;
    this.account = null;
    this.cursor = 0;
    this.job = null;
    this.running = false;
    this.busy = false;
    this.generation = 0;
    this.inputQueue = Promise.resolve();
    this.byId = (id) => document.getElementById(`update-${id}`);
    this.byId("button").addEventListener("click", () => {
      this.byId("dialog").showModal();
      this.refresh(true);
      if (this.terminal) this.terminal.focus();
    });
    this.byId("check").addEventListener("click", () => this.refresh(true));
    this.byId("install").addEventListener("click", async () => {
      if (!this.latest || !confirm(`Install Xedoc ${this.latest}?`)) return;
      this.byId("install").disabled = true;
      try {
        await this.api("update/start", { tag: `v${this.latest}` });
        this.cursor = 0;
        this.job = null;
        this.running = true;
        this.terminal?.reset();
        await this.refresh();
        this.terminal?.focus();
      } catch (error) {
        this.byId("status").textContent = error.message;
      } finally {
        this.byId("install").disabled = false;
      }
    });
    const keys = { up: "\x1b[A", down: "\x1b[B", tab: "\t", escape: "\x1b", enter: "\r" };
    document.querySelectorAll("[data-update-key]").forEach((button) => {
      button.addEventListener("click", () => {
        this.send(keys[button.dataset.updateKey]);
        this.terminal?.focus();
      });
    });
    window.setInterval(() => {
      if (this.running || this.byId("dialog").open) this.refresh();
    }, 750);
    window.setInterval(() => this.refresh(true), 60 * 60 * 1000);
  }

  setAccount(account) {
    const name = account?.role === "admin" ? account.username : null;
    this.byId("button").hidden = !name;
    if (name === this.account) return;
    this.account = name;
    this.generation++;
    this.cursor = 0;
    this.job = null;
    this.running = false;
    this.latest = null;
    this.terminal?.reset();
    if (name) this.refresh(true);
  }

  send(data) {
    const generation = this.generation;
    this.inputQueue = this.inputQueue.then(async () => {
      if (!this.running || generation !== this.generation) return;
      try {
        await this.api("update/input", { data });
      } catch (error) {
        this.byId("status").textContent = error.message;
      }
    });
  }

  async refresh(check = false) {
    if (!this.account || this.busy) return;
    this.busy = true;
    const generation = this.generation;
    try {
      const requestedCursor = this.cursor;
      const result = await this.api(`update/status?cursor=${this.cursor}`);
      if (generation !== this.generation) return;
      this.byId("version").textContent = `Installed ${result.currentVersion || "unknown"}`;
      const job = result.pid ? `${result.pid}:${result.tag}` : null;
      if (job !== this.job) {
        this.job = job;
        this.cursor = 0;
        this.terminal?.reset();
        if (requestedCursor > 0) return;
      }
      const wasRunning = this.running;
      this.running = ["starting", "running"].includes(result.status);
      this.byId("check").disabled = this.running;
      this.byId("install").disabled = this.running;
      const messages = {
        idle: this.latest ? `Latest ${this.latest}` : "Ready",
        starting: "Starting installer…",
        running: "Installer running",
        completed: "Update completed",
        failed: `Installer failed${result.exitCode == null ? "" : ` (${result.exitCode})`}`,
        interrupted: "Installer stopped unexpectedly",
      };
      this.byId("status").textContent = result.supported
        ? messages[result.status] : "Updates require a macOS or Linux host";
      if (this.latest === result.currentVersion) this.byId("install").hidden = true;
      if (this.running) this.byId("button").title = "Updating…";
      if (job) {
        this.byId("terminal").hidden = false;
        this.byId("keys").hidden = !this.running;
        if (!this.terminal) {
          this.terminal = new window.Terminal({
            cols: 100, rows: 30, fontSize: 12, scrollback: 2000,
            theme: { background: "#141a1f", foreground: "#cad7e0", cursor: "#bd82f5" },
          });
          this.terminal.open(this.byId("terminal"));
          this.terminal.onData((data) => this.send(data));
        }
        if (result.reset) this.terminal.reset();
        const bytes = Uint8Array.from(atob(result.output), (character) => character.charCodeAt(0));
        if (bytes.length) this.terminal.write(bytes);
      }
      this.cursor = result.cursor;
      if (wasRunning && result.status === "completed") {
        window.setTimeout(() => window.location.reload(), 250);
        return;
      }
      if (check && !this.running) {
        this.byId("status").textContent = "Checking for updates…";
        const release = await this.api("update/check");
        if (generation !== this.generation) return;
        this.latest = release.latestVersion;
        this.byId("install").textContent = `Install ${release.latestVersion}`;
        this.byId("install").hidden = !release.available || !release.supported;
        this.byId("button").title = release.available
          ? `Update · ${release.latestVersion}` : "Updates";
        this.byId("status").textContent = result.status === "idle"
          ? `Latest ${release.latestVersion}` : messages[result.status];
      } else if (!this.running) {
        this.byId("button").title = this.byId("install").hidden
          ? "Updates" : `Update · ${this.latest}`;
      }
      this.byId("button").setAttribute("aria-label", this.byId("button").title);
    } catch (error) {
      if (generation === this.generation) {
        this.byId("status").textContent = this.running
          ? "Reconnecting to host… Installer continues running."
          : error.message;
      }
    } finally {
      this.busy = false;
    }
  }
};
