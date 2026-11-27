"use strict";

window.XedocBrowserAutomation = class {
  constructor(api, panels, sessions) {
    this.api = api;
    this.panels = panels;
    this.sessions = sessions;
    this.pending = new Map();
    window.xedocBrowserResult = (response) => {
      const pending = this.pending.get(response?.requestId);
      if (!pending) return;
      this.pending.delete(response.requestId);
      clearTimeout(pending.timer);
      if (response.error) pending.reject(new Error(response.error));
      else pending.resolve(response.result);
    };
    if (window.webkit?.messageHandlers?.xedocDesktop) {
      void this.poll();
    }
  }

  async poll() {
    while (true) {
      const contexts = this.sessions.contexts();
      if (!contexts.length) {
        await new Promise((resolve) => setTimeout(resolve, 500));
        continue;
      }
      try {
        const command = await this.api("browser/poll", {
          ...this.panels.params(contexts[0]),
          sessions: contexts.map((context) => this.panels.params(context)),
        });
        if (command.command) {
          const context = contexts.find((item) => item.threadId === (command.command.threadId || contexts[0].threadId));
          if (context) void this.complete(command.command, context).catch(() => {});
        }
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    }
  }

  async complete(command, context) {
    let result = null;
    let error = null;
    try {
      result = await this.execute(command, context);
    } catch (cause) {
      error = String(cause?.message || cause || "Browser automation failed.").slice(0, 512);
    }
    await this.api("browser/result", {
      ...this.panels.params(context), requestId: command.requestId, result, error,
    });
  }

  async execute(command, context) {
    const contextKey = this.panels.contextKey(context);
    if (!this.sessions.contexts().some((item) => this.panels.contextKey(item) === contextKey)) {
      throw new Error("Browser session was closed before command execution.");
    }
    const { action, arguments: args } = command;
    if (action === "open") {
      const presentation = args.presentation || "foreground";
      if (presentation === "foreground") await this.sessions.reveal(context);
      this.panels.sync();
      return { panelId: this.panels.openWebsite(args.url, true, { key: contextKey, presentation }) };
    }
    if (action === "list") {
      return this.panels.listWebsites(contextKey);
    }
    if (action === "set_presentation") {
      this.panels.getWebsite(args.panelId, contextKey);
      if (args.presentation === "foreground") await this.sessions.reveal(context);
      this.panels.sync();
      return this.panels.setWebsitePresentation(args.panelId, args.presentation, contextKey);
    }
    if (action === "extensions") {
      return await this.native({
        kind: "browserAutomation",
        requestId: command.requestId,
        action,
        input: args,
      }, command.timeout);
    }
    if (action === "close") {
      return this.panels.closeWebsite(args.panelId, contextKey);
    }
    this.panels.getWebsite(args.panelId, contextKey);
    const event = {
      kind: "browserAutomation",
      requestId: command.requestId,
      panelId: args.panelId,
      action,
      url: args.url,
      script: this.script(action, args),
      input: ["pointer", "key", "type", "cdp", "cdp_events", "save_download"].includes(action) ? args : undefined,
    };
    return await this.native(event, command.timeout);
  }

  native(event, timeout) {
    const bridge = window.webkit?.messageHandlers?.xedocDesktop;
    if (!bridge) throw new Error("Native website panels are unavailable in this Host GUI.");
    return new Promise((resolve, reject) => {
      const requestId = event.requestId;
      const timer = setTimeout(() => {
        this.pending.delete(requestId);
        reject(new Error("Native browser request timed out."));
      }, timeout * 1000 + 1000);
      this.pending.set(requestId, { resolve, reject, timer });
      try {
        bridge.postMessage(event);
      } catch (error) {
        clearTimeout(timer);
        this.pending.delete(requestId);
        reject(error);
      }
    });
  }

  script(action, args) {
    if (["navigate", "back", "forward", "reload", "screenshot", "pointer", "key", "type", "cdp", "cdp_events", "downloads", "save_download"].includes(action)) return null;
    if (action === "evaluate" && !args.framePath?.length) return `(${args.script})`;
    return `(${window.XedocBrowserPage.toString()})(${JSON.stringify(action)},${JSON.stringify(args)})`;
  }
};
