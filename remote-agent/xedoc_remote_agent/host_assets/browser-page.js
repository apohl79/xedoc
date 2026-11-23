"use strict";

// Serialized into the website's page world by both native clients. No host bridge is exposed.
window.XedocBrowserPage = async function (action, args) {
  let doc = document;
  function query(root, selector) {
    const parts = selector.split(" >>> ");
    for (let index = 0; index < parts.length - 1; index++) {
      root = root.querySelector(parts[index])?.shadowRoot;
      if (!root) throw new Error("Shadow root is missing or closed.");
    }
    return { root, selector: parts.at(-1) };
  }
  function find(selector, root = doc) {
    const target = query(root, selector);
    return target.root.querySelector(target.selector);
  }
  for (const selector of args.framePath || []) {
    const frame = find(selector);
    try {
      if (!frame?.contentDocument) throw new Error();
      doc = frame.contentDocument;
    } catch {
      throw new Error("Frame is missing, not loaded, or cross-origin. Cross-origin frame targeting is unsupported.");
    }
  }
  const win = doc.defaultView;
  const clip = (value, length = 512) => String(value ?? "").slice(0, length);
  const required = () => {
    const node = find(args.selector);
    if (!node) throw new Error("No element matches the selector.");
    return node;
  };
  function selectorFor(node) {
    const parts = [];
    while (node?.nodeType === 1) {
      if (node.id) {
        parts.unshift(`#${win.CSS.escape(node.id)}`);
        break;
      }
      const siblings = [...(node.parentNode?.children || [])].filter((item) => item.tagName === node.tagName);
      parts.unshift(`${node.localName}:nth-of-type(${siblings.indexOf(node) + 1})`);
      node = node.parentElement;
    }
    return parts.join(" > ");
  }
  const stateKey = Symbol.for("xedoc.browser.observer");
  if (action === "observe") {
    win[stateKey]?.stop();
    if (args.enabled === false) {
      delete win[stateKey];
      return { recording: false };
    }
    const state = { console: [], network: [], sequence: 0, startedAt: Date.now() };
    win[stateKey] = state;
    const add = (kind, entry) => {
      Object.assign(entry, { id: ++state.sequence, at: Date.now() });
      state[kind].push(entry);
      if (state[kind].length > 50) state[kind].shift();
    };
    const restores = [];
    for (const level of ["log", "info", "warn", "error", "debug"]) {
      const original = win.console[level];
      const replacement = function (...values) {
        add("console", { level, text: values.slice(0, 8).map((value) => {
          if (value === null || typeof value !== "object") return clip(value, 256);
          return value instanceof win.Error ? clip(value.stack || value.message) : "[object]";
        }).join(" ").slice(0, 512) });
        return Reflect.apply(original, this, values);
      };
      win.console[level] = replacement;
      restores.push(() => { if (win.console[level] === replacement) win.console[level] = original; });
    }
    const onError = (event) => add("console", { level: "error", text: clip(event.message), source: clip(event.filename), line: event.lineno });
    const onRejection = (event) => add("console", { level: "error", text: clip(event.reason?.message || event.reason) });
    win.addEventListener("error", onError);
    win.addEventListener("unhandledrejection", onRejection);
    restores.push(() => {
      win.removeEventListener("error", onError);
      win.removeEventListener("unhandledrejection", onRejection);
    });
    if (typeof win.fetch === "function") {
      const original = win.fetch;
      const replacement = async function (input, options) {
        const entry = {
          transport: "fetch", url: clip(typeof input === "string" || input instanceof win.URL ? input : input?.url),
          method: clip(options?.method || input?.method || "GET", 16),
        };
        const started = win.performance.now();
        try {
          const response = await Reflect.apply(original, this, [input, options]);
          Object.assign(entry, { status: response.status, durationMs: Math.round(win.performance.now() - started) });
          add("network", entry);
          if (args.captureBodies && /json|text|javascript|xml/.test(response.headers.get("content-type") || "")) {
            // Read only a bounded prefix of a clone, never consume the application's response.
            void (async () => {
              const reader = response.clone().body?.getReader();
              if (!reader) return;
              const decoder = new win.TextDecoder();
              let text = "";
              try {
                while (text.length < 2048) {
                  const chunk = await reader.read();
                  if (chunk.done) break;
                  text += decoder.decode(chunk.value.subarray(0, 8192), { stream: true });
                }
                const recorded = state.network.find((item) => item.id === entry.id);
                if (recorded) recorded.body = text.slice(0, 2048);
              } finally {
                void reader.cancel().catch(() => {});
              }
            })().catch(() => {});
          }
          return response;
        } catch (error) {
          add("network", { ...entry, error: clip(error?.message), durationMs: Math.round(win.performance.now() - started) });
          throw error;
        }
      };
      win.fetch = replacement;
      restores.push(() => { if (win.fetch === replacement) win.fetch = original; });
    }
    const prototype = win.XMLHttpRequest.prototype;
    const originalOpen = prototype.open;
    const originalSend = prototype.send;
    const requests = new WeakMap();
    const open = function (method, url, ...rest) {
      requests.set(this, { transport: "xhr", method: clip(method, 16), url: clip(url) });
      return Reflect.apply(originalOpen, this, [method, url, ...rest]);
    };
    const send = function (...values) {
      const entry = requests.get(this);
      const started = win.performance.now();
      if (entry) this.addEventListener("loadend", () => {
        if (win[stateKey] !== state) return;
        const result = { ...entry, status: this.status, durationMs: Math.round(win.performance.now() - started) };
        if (args.captureBodies && (this.responseType === "" || this.responseType === "text")) {
          result.body = this.responseText.slice(0, 2048);
        }
        add("network", result);
      }, { once: true });
      return Reflect.apply(originalSend, this, values);
    };
    prototype.open = open;
    prototype.send = send;
    restores.push(() => {
      if (prototype.open === open) prototype.open = originalOpen;
      if (prototype.send === send) prototype.send = originalSend;
    });
    state.stop = () => restores.forEach((restore) => restore());
    return {
      recording: true, startedAt: state.startedAt, captureBodies: args.captureBodies === true,
      limits: "Last 50 console and fetch/XHR events in this document, after observe. Bodies opt-in, 2048 characters. Navigation resets recording. No headers, request bodies, WebSockets, workers, or browser-level capture.",
    };
  }
  if (action === "diagnostics") {
    const state = win[stateKey];
    if (args.kind === "resources") {
      const entries = win.performance.getEntriesByType("resource");
      return { total: entries.length, entries: entries.slice(args.offset || 0, (args.offset || 0) + (args.limit || 5))
        .map((item) => ({ url: clip(item.name), type: item.initiatorType, durationMs: Math.round(item.duration), transferSize: item.transferSize })) };
    }
    if (!state) throw new Error("Recording is not active in this document. Call observe first; navigation resets it.");
    const entries = state[args.kind || "console"];
    const filtered = args.id === undefined ? entries : entries.filter((item) => item.id === args.id);
    return { startedAt: state.startedAt, total: filtered.length, entries: filtered.slice(args.offset || 0, (args.offset || 0) + (args.limit || 2)) };
  }
  switch (action) {
    case "evaluate":
      return await win.eval(`(${args.script})`);
    case "frames":
      return [...doc.querySelectorAll("iframe,frame")].slice(0, 30).map((node) => {
        let accessible = false;
        try { accessible = !!node.contentDocument; } catch {}
        return { selector: selectorFor(node), url: clip(node.src), name: clip(node.name, 128), accessible };
      });
    case "snapshot": {
      const root = args.selector ? required() : doc.body;
      const elements = [];
      function visit(scope, prefix = "") {
        for (const node of scope?.querySelectorAll("*") || []) {
          if (elements.length >= (args.offset || 0) + (args.limit || 12) + 1) return;
          if (node.matches("a[href],button,input,textarea,select,[role],[tabindex],summary")) {
            elements.push({
              selector: clip(prefix + selectorFor(node), 512), tag: node.localName,
              role: node.getAttribute("role"), type: node.getAttribute("type"),
              text: clip(node.innerText || node.getAttribute("aria-label") || node.getAttribute("placeholder"), 80),
              disabled: node.disabled === true,
            });
          }
          if (node.shadowRoot) visit(node.shadowRoot, prefix + selectorFor(node) + " >>> ");
        }
      }
      visit(root);
      const offset = args.offset || 0;
      const limit = args.limit || 12;
      return {
        title: clip(doc.title, 256), url: clip(doc.URL),
        text: clip(root?.innerText, args.textLimit ?? 1500),
        elements: elements.slice(offset, offset + limit),
        nextOffset: elements.length > offset + limit ? offset + limit : null,
      };
    }
    case "inspect": {
      const node = required();
      const rect = node.getBoundingClientRect();
      const style = win.getComputedStyle(node);
      const clone = node.cloneNode(true);
      for (const input of [clone, ...clone.querySelectorAll("input,textarea")]) {
        if (input.matches("input")) input.removeAttribute("value");
        if (input.matches("textarea")) input.textContent = "[redacted]";
      }
      return {
        tag: node.localName, text: clip(node.innerText, 512),
        html: clip(clone.outerHTML, 2048),
        bounds: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
        styles: Object.fromEntries(["display", "visibility", "position", "color", "background-color", "font-size", "z-index", "pointer-events"]
          .map((name) => [name, style.getPropertyValue(name)])),
        focused: doc.activeElement === node, disabled: node.disabled === true,
        checked: typeof node.checked === "boolean" ? node.checked : null,
      };
    }
    case "wait_for": {
      const deadline = Date.now() + (args.timeoutMs || 5000);
      while (true) {
        const node = args.selector ? find(args.selector) : doc.body;
        const rect = node?.getBoundingClientRect();
        const visible = !!rect && rect.width > 0 && rect.height > 0 && win.getComputedStyle(node).visibility !== "hidden";
        const matches = args.state === "hidden" ? !visible
          : !!node && (args.state !== "visible" || visible) && (!args.text || node.textContent.includes(args.text));
        if (matches) return { matched: true };
        if (Date.now() >= deadline) throw new Error("Timed out waiting for the requested page state.");
        await new Promise((resolve) => setTimeout(resolve, 100));
      }
    }
    case "click": {
      const node = required();
      node.scrollIntoView({ block: "center", inline: "center" });
      node.click();
      return { clicked: true, trusted: false };
    }
    case "focus":
      required().focus();
      return { focused: true };
    case "fill": {
      const node = required();
      if (!(node instanceof win.HTMLInputElement || node instanceof win.HTMLTextAreaElement)) throw new Error("Selector must match an input or textarea.");
      if (node.type === "password" || node.type === "file") throw new Error("Password and file inputs cannot be filled.");
      const prototype = node instanceof win.HTMLTextAreaElement ? win.HTMLTextAreaElement.prototype : win.HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(prototype, "value").set.call(node, args.value);
      node.dispatchEvent(new win.InputEvent("input", { bubbles: true, inputType: "insertText", data: args.value }));
      node.dispatchEvent(new win.Event("change", { bubbles: true }));
      return { filled: true };
    }
    case "select": {
      const node = required();
      if (!(node instanceof win.HTMLSelectElement) || ![...node.options].some((option) => option.value === args.value)) throw new Error("Selector must match a select with the requested option.");
      node.value = args.value;
      node.dispatchEvent(new win.Event("input", { bubbles: true }));
      node.dispatchEvent(new win.Event("change", { bubbles: true }));
      return { selected: node.value };
    }
    case "scroll": {
      const node = args.selector ? required() : win;
      node.scrollBy({ left: args.x || 0, top: args.y || 0, behavior: "auto" });
      return { scrolled: true };
    }
    default:
      throw new Error("Unsupported page operation.");
  }
};
