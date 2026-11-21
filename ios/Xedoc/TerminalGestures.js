(() => {
  const style = new CSSStyleSheet();
  style.replaceSync(`
    .terminal-frame.mobile-terminal-pane {
      position: relative; padding-bottom: calc(56px + env(safe-area-inset-bottom));
    }
    .mobile-terminal-keys {
      position: absolute; bottom: 6px; left: 8px; right: 8px;
      display: flex; gap: 4px;
      padding-bottom: env(safe-area-inset-bottom);
    }
    .mobile-terminal-keys button {
      flex: 1; min-width: 0; min-height: 44px; padding: 4px;
      font: inherit; font-size: 14px; color: inherit;
      border: 1px solid var(--border); border-radius: 4px;
      background: var(--bg-prompt, #252b32); touch-action: manipulation;
    }
    .mobile-terminal-keys button[aria-pressed="true"] {
      background: #5ec4ff; color: #111820; border-color: #5ec4ff;
    }
  `);
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, style];
  const keyCodes = {
    Backspace: 8, Tab: 9, Enter: 13, PageUp: 33, PageDown: 34, Delete: 46,
    "!": 49, "@": 50, "#": 51, "$": 52, "%": 53, "^": 54, "&": 55, "*": 56,
    "(": 57, ")": 48, "-": 189, "_": 189, "=": 187, "+": 187,
    "[": 219, "{": 219, "]": 221, "}": 221, "\\": 220, "|": 220,
    ";": 186, ":": 186, "'": 222, '"': 222, ",": 188, "<": 188,
    ".": 190, ">": 190, "/": 191, "?": 191, "`": 192, "~": 192,
  };
  function installKeys(pane) {
    if (pane.querySelector(".mobile-terminal-keys")) return;
    const input = pane.querySelector(".xterm-helper-textarea");
    if (!input) return;
    const modifiers = { ctrlKey: false, altKey: false, metaKey: false };
    const generatedKeys = new WeakSet();
    const bar = document.createElement("div");
    bar.className = "mobile-terminal-keys";
    bar.setAttribute("role", "group");
    bar.setAttribute("aria-label", "Terminal special keys");
    const buttons = new Map();
    function reset() {
      for (const name of buttons.keys()) {
        modifiers[name] = false;
        buttons.get(name).setAttribute("aria-pressed", "false");
      }
    }
    function key(value, options = {}) {
      const event = new KeyboardEvent("keydown", {
        bubbles: true, cancelable: true, ...options, key: value,
        keyCode: options.keyCode || keyCodes[value] || value.toUpperCase().charCodeAt(0),
        shiftKey: options.shiftKey || /^[A-Z~!@#$%^&*()_+{}|:"<>?]$/.test(value),
        ctrlKey: options.ctrlKey || modifiers.ctrlKey,
        altKey: options.altKey || modifiers.altKey,
        metaKey: options.metaKey || modifiers.metaKey,
      });
      generatedKeys.add(event);
      input.dispatchEvent(event);
      reset();
    }
    for (const [label, modifier, value] of [
      ["Ctrl", "ctrlKey"], ["Alt", "altKey"], ["Cmd", "metaKey"],
      ["Tab", null, "Tab"], ["PgUp", null, "PageUp"], ["PgDown", null, "PageDown"],
    ]) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = label;
      button.setAttribute("aria-label", { PgUp: "Page Up", PgDown: "Page Down",
        Ctrl: "Control", Cmd: "Command" }[label] || label);
      if (modifier) {
        buttons.set(modifier, button);
        button.setAttribute("aria-pressed", "false");
      }
      button.addEventListener("pointerdown", (event) => event.preventDefault());
      const activate = () => {
        if (modifier) {
          modifiers[modifier] = !modifiers[modifier];
          button.setAttribute("aria-pressed", String(modifiers[modifier]));
        } else {
          key(value);
        }
        input.focus({ preventScroll: true });
      };
      button.addEventListener("click", activate);
      button.addEventListener("touchend", (event) => {
        event.preventDefault();
        activate();
      }, { passive: false });
      bar.append(button);
    }
    const active = () => Object.values(modifiers).some(Boolean);
    input.addEventListener("beforeinput", (event) => {
      if (!active() || event.isComposing || !event.cancelable) return;
      const value = { deleteContentBackward: "Backspace", deleteContentForward: "Delete",
        insertParagraph: "Enter", insertLineBreak: "Enter" }[event.inputType]
        || (event.inputType === "insertText" && event.data?.length === 1 ? event.data : null);
      if (!value) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      key(value);
    }, true);
    pane.addEventListener("keydown", (event) => {
      if (event.target !== input || generatedKeys.has(event) || !active() || event.isComposing
          || ["Control", "Alt", "Meta", "Shift", "Unidentified"].includes(event.key)) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      key(event.key, {
        keyCode: event.keyCode, shiftKey: event.shiftKey,
        ctrlKey: event.ctrlKey || modifiers.ctrlKey,
        altKey: event.altKey || modifiers.altKey, metaKey: event.metaKey || modifiers.metaKey,
      });
    }, true);
    new MutationObserver(() => { if (pane.hidden) reset(); })
      .observe(pane, { attributes: true, attributeFilter: ["hidden"] });
    pane.classList.add("mobile-terminal-pane");
    pane.append(bar);
  }
  const install = () => document.querySelectorAll(".terminal-frame").forEach(installKeys);
  new MutationObserver(install).observe(document.documentElement, { childList: true, subtree: true });
  install();
  window.xedocMobileTerminalAtPoint = (x, y) => {
    const target = document.elementFromPoint(x, y);
    if (target?.closest(".terminal-frame, #live")) return true;
    if (target?.tagName !== "IFRAME") return false;
    const rect = target.getBoundingClientRect();
    try {
      return target.contentWindow.xedocMobileTerminalAtPoint?.(
        x - rect.left - target.clientLeft, y - rect.top - target.clientTop) || false;
    } catch {
      return false;
    }
  };
  let swipe;
  document.addEventListener("touchstart", (event) => {
    swipe = null;
    if (event.touches.length !== 1 || !window.getSelection()?.isCollapsed) return;
    if (event.target.closest(".mobile-terminal-keys")) return;
    const pane = event.target.closest(".terminal-frame, #live");
    if (!pane || pane.hidden) return;
    const input = pane.matches(".terminal-frame")
      ? pane.querySelector(".xterm-helper-textarea") : pane;
    if (!input) return;
    const touch = event.touches[0];
    swipe = { pane, input, id: touch.identifier, x: touch.clientX, y: touch.clientY,
      time: performance.now(), direction: null };
  }, { capture: true, passive: true });
  document.addEventListener("touchmove", (event) => {
    if (!swipe) return;
    const touch = [...event.touches].find((touch) => touch.identifier === swipe.id);
    if (event.touches.length !== 1 || !touch || performance.now() - swipe.time > 600
        || !window.getSelection()?.isCollapsed || swipe.pane.hidden) {
      swipe = null;
      return;
    }
    const dx = touch.clientX - swipe.x;
    const dy = touch.clientY - swipe.y;
    swipe.direction = null;
    const horizontal = Math.abs(dx) >= Math.abs(dy) * 2;
    const vertical = Math.abs(dy) >= Math.abs(dx) * 2;
    const distance = horizontal ? Math.abs(dx) : Math.abs(dy);
    if ((!horizontal && !vertical) || distance < 10) return;
    if (!event.cancelable) {
      swipe = null;
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (distance >= 40) {
      swipe.direction = horizontal
        ? (dx < 0 ? "ArrowLeft" : "ArrowRight")
        : (dy < 0 ? "ArrowUp" : "ArrowDown");
    }
  }, { capture: true, passive: false });
  document.addEventListener("touchend", (event) => {
    const gesture = swipe;
    swipe = null;
    if (!gesture?.direction || event.touches.length || gesture.pane.hidden
        || !gesture.input.isConnected || performance.now() - gesture.time > 600) return;
    event.preventDefault();
    event.stopPropagation();
    const keyCode = { ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40 }[gesture.direction];
    gesture.input.dispatchEvent(new KeyboardEvent("keydown", {
      key: gesture.direction, keyCode, bubbles: true, cancelable: true,
    }));
  }, { capture: true, passive: false });
  document.addEventListener("touchcancel", () => { swipe = null; }, { capture: true });
})();
