"use strict";

(() => {
  const tools = document.getElementById("plugin-tools");
  if (!tools || window.parent === window) return;
  const menu = document.createElement("div");
  menu.id = "plugin-composer-menu";
  menu.setAttribute("role", "menu");
  menu.hidden = true;
  document.body.append(menu);
  let declarations = "";
  let activeButton = null;
  let pending = null;

  function close() {
    menu.hidden = true;
    activeButton?.setAttribute("aria-expanded", "false");
    activeButton = null;
  }

  function position() {
    if (menu.hidden || !activeButton) return;
    const viewport = window.visualViewport;
    const left = viewport?.offsetLeft || 0;
    const top = viewport?.offsetTop || 0;
    const width = viewport?.width || innerWidth;
    const height = viewport?.height || innerHeight;
    const bounds = activeButton.getBoundingClientRect();
    menu.style.maxHeight = `${Math.max(0, Math.min(height / 2, bounds.top - top - 16))}px`;
    menu.style.left = `${Math.max(left + 8, Math.min(bounds.left, left + width - menu.offsetWidth - 8))}px`;
    menu.style.top = `${Math.max(top + 8, bounds.top - menu.offsetHeight - 8)}px`;
  }

  function finish() {
    if (!pending) return;
    clearTimeout(pending.timer);
    pending.button.disabled = false;
    pending.button.removeAttribute("aria-busy");
    pending = null;
  }

  function request(button, declaration, target) {
    if (pending) return;
    const requestId = crypto.randomUUID();
    button.disabled = true;
    button.setAttribute("aria-busy", "true");
    const timer = setTimeout(() => {
      finish();
      if (activeButton === button) {
        menu.textContent = "Plugin action timed out. Try again.";
        menu.hidden = false;
        position();
      }
    }, 240000);
    pending = { requestId, button, declaration, timer, selecting: target !== undefined };
    window.parent.postMessage({
      type: "xedoc-plugin-composer", requestId, pluginId: declaration.pluginId,
      panelId: declaration.panelId, buttonId: declaration.buttonId,
      ...(target === undefined ? {} : { target }),
    }, location.origin);
  }

  function draw(result, button, declaration) {
    menu.replaceChildren();
    function append(items, parent) {
      for (const item of items) {
        if (item.children) {
          const directory = document.createElement("details");
          directory.open = item.expanded === true;
          const label = document.createElement("summary");
          label.textContent = item.title;
          directory.append(label);
          const children = document.createElement("div");
          children.className = "plugin-menu-children";
          append(item.children, children);
          directory.append(children);
          directory.addEventListener("toggle", position);
          parent.append(directory);
        } else {
          const file = document.createElement("button");
          file.type = "button";
          file.setAttribute("role", "menuitem");
          file.textContent = item.title;
          file.title = item.target;
          file.addEventListener("click", () => {
            if (pending) return;
            request(button, declaration, item.target);
            menu.replaceChildren(document.createTextNode("Opening…"));
            position();
          });
          parent.append(file);
        }
      }
    }
    append(result.items || [], menu);
    if (!menu.childElementCount) {
      menu.textContent = result.emptyMessage || "No items available";
    }
    position();
    menu.querySelector("summary, button")?.focus({ preventScroll: true });
  }

  window.addEventListener("message", (event) => {
    if (event.origin !== location.origin || event.source !== window.parent) return;
    const data = event.data;
    if (data?.type === "xedoc-plugin-actions") {
      const buttons = Array.isArray(data.composerButtons) ? data.composerButtons : [];
      const signature = JSON.stringify(buttons);
      if (signature === declarations) return;
      declarations = signature;
      finish();
      close();
      tools.replaceChildren();
      for (const declaration of buttons) {
        const button = document.createElement("button");
        button.type = "button";
        button.title = declaration.title;
        button.setAttribute("aria-label", declaration.title);
        button.textContent = `${declaration.icon || "⚙"} ${declaration.title}`;
        if (declaration.mode === "menu") {
          button.setAttribute("aria-haspopup", "menu");
          button.setAttribute("aria-expanded", "false");
        }
        button.addEventListener("click", () => {
          if (activeButton === button) { close(); return; }
          if (pending) return;
          document.dispatchEvent(new Event("xedoc-plugin-menu"));
          close();
          activeButton = button;
          if (declaration.mode === "menu") {
            menu.setAttribute("aria-label", declaration.title);
            button.setAttribute("aria-expanded", "true");
            menu.textContent = "Loading…";
            menu.hidden = false;
            position();
          }
          request(button, declaration);
        });
        tools.append(button);
      }
    }
    if (data?.type !== "xedoc-plugin-composer-result" || data.requestId !== pending?.requestId) return;
    const { button, declaration, selecting } = pending;
    finish();
    if (activeButton !== button) return;
    if (data.error) {
      menu.textContent = data.error;
      menu.setAttribute("aria-label", declaration.title);
      menu.hidden = false;
      position();
    } else if (declaration.mode === "menu" && !selecting) {
      draw(data, button, declaration);
    } else {
      close();
      button.focus({ preventScroll: true });
    }
  });
  document.addEventListener("pointerdown", (event) => {
    if (!menu.contains(event.target) && !tools.contains(event.target)) close();
  });
  document.addEventListener("focusin", (event) => {
    if (!menu.contains(event.target) && !tools.contains(event.target)) close();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && activeButton) {
      const button = activeButton;
      close();
      button.focus({ preventScroll: true });
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  }, true);
  menu.addEventListener("keydown", (event) => {
    if (event.key === "Tab") {
      const button = activeButton;
      close();
      button?.focus({ preventScroll: true });
      event.preventDefault();
      return;
    }
    if (["ArrowLeft", "ArrowRight"].includes(event.key)) {
      const directory = document.activeElement.closest("details");
      if (directory) {
        directory.open = event.key === "ArrowRight";
        if (!directory.open) directory.querySelector("summary").focus();
        event.preventDefault();
      }
      return;
    }
    if (!["ArrowDown", "ArrowUp"].includes(event.key)) return;
    const items = [...menu.querySelectorAll("summary, button")].filter(item => item.getClientRects().length);
    const index = items.indexOf(document.activeElement);
    items[(index + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
    event.preventDefault();
  });
  window.addEventListener("resize", position);
  window.visualViewport?.addEventListener("resize", position);
})();
