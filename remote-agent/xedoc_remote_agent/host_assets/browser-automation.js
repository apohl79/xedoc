"use strict";

window.XedocBrowserAutomation = class {
  constructor(api, panels) {
    this.api = api;
    this.panels = panels;
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
      const context = this.panels.context();
      if (!context) {
        await new Promise((resolve) => setTimeout(resolve, 500));
        continue;
      }
      const contextKey = this.panels.contextKey(context);
      try {
        const command = await this.api("browser/poll", {
          workspaceId: context.workspaceId,
          projectName: context.projectName,
          threadId: context.threadId,
        });
        if (command.command) {
          let result = null;
          let error = null;
          try {
            result = await this.execute(command.command, contextKey);
          } catch (cause) {
            error = String(cause?.message || cause || "Browser automation failed.").slice(0, 512);
          }
          await this.api("browser/result", {
            workspaceId: context.workspaceId,
            projectName: context.projectName,
            threadId: context.threadId,
            requestId: command.command.requestId,
            result,
            error,
          });
        }
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    }
  }

  async execute(command, contextKey) {
    if (this.panels.contextKey(this.panels.context()) !== contextKey) {
      throw new Error("Active session changed before browser command execution.");
    }
    const { action, arguments: args } = command;
    if (action === "open") {
      return { panelId: this.panels.openWebsite(args.url, true) };
    }
    if (action === "list") {
      return this.panels.listWebsites();
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
      return this.panels.closeWebsite(args.panelId);
    }
    this.panels.getWebsite(args.panelId);
    const event = {
      kind: "browserAutomation",
      requestId: command.requestId,
      panelId: args.panelId,
      action,
      url: args.url,
      script: this.script(action, args),
      input: ["pointer", "key", "type", "cdp", "cdp_events"].includes(action) ? args : undefined,
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
    if (["navigate", "back", "forward", "reload", "screenshot", "pointer", "key", "type", "cdp", "cdp_events"].includes(action)) return null;
    if (action === "evaluate" && !args.framePath?.length) return `(${args.script})`;
    return `(${window.XedocBrowserPage.toString()})(${JSON.stringify(action)},${JSON.stringify(args)})`;
  }
};
