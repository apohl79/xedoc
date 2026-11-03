(() => {
  let docked = false;
  let hostView;
  let lock;
  let sidebar;
  function open(show) {
    if (!hostView) return;
    hostView.classList.toggle("sidebar-open", show);
    document.getElementById("sidebar-toggle").setAttribute("aria-expanded", String(show));
  }
  function layout() {
    if (!lock) return;
    if ((lock.getAttribute("aria-pressed") === "true") !== docked) lock.click();
    open(docked);
  }
  window.xedocMobileSidebar = (action, landscape) => {
    if (action === "layout") {
      docked = landscape;
      layout();
    } else {
      open(docked || action === "show");
    }
  };
  window.addEventListener("load", () => {
    const scrollingStyle = document.createElement("style");
    scrollingStyle.textContent = "* { touch-action: pan-y !important; }";
    document.head.append(scrollingStyle);
    if (window !== window.top) {
      document.addEventListener("pointerdown", () => {
        window.top.xedocMobileSidebar?.("hide");
      }, true);
    }
    document.querySelectorAll('input, select, textarea, [contenteditable]').forEach((input) => {
      input.style.setProperty("font-size", "16px", "important");
    });
    const send = document.getElementById("send");
    const input = document.getElementById("input");
    if (send && input) {
      const menu = document.createElement("div");
      menu.id = "mobile-send-menu";
      menu.popover = "manual";
      menu.role = "menu";
      menu.setAttribute("aria-label", "Message actions");
      menu.style.cssText = "position:fixed;inset:auto;margin:0;padding:4px;width:160px;"
        + "border:1px solid var(--border);border-radius:8px;background:var(--bg-prompt);"
        + "color:inherit;box-shadow:0 8px 24px #0008";
      send.setAttribute("aria-haspopup", "menu");
      send.setAttribute("aria-controls", menu.id);
      send.style.webkitTouchCallout = "none";
      send.style.userSelect = "none";
      for (const [label, key] of [["Send", "Enter"], ["Queue", "Tab"]]) {
        const item = document.createElement("button");
        item.type = "button";
        item.role = "menuitem";
        item.textContent = label;
        item.style.cssText = "display:block;width:100%;min-height:44px;text-align:left;"
          + "padding:8px 12px;font-size:16px;background:transparent;color:inherit";
        item.addEventListener("click", () => {
          menu.hidePopover();
          input.focus({ preventScroll: true });
          input.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
        });
        menu.append(item);
      }
      document.body.append(menu);
      let timer;
      let held = false;
      let start;
      const cancel = () => clearTimeout(timer);
      const show = () => {
        if (send.disabled) return;
        held = true;
        const rect = send.getBoundingClientRect();
        menu.style.right = `${Math.max(8, innerWidth - rect.right)}px`;
        menu.style.bottom = `${innerHeight - rect.top + 8}px`;
        menu.showPopover();
      };
      send.addEventListener("pointerdown", (event) => {
        if (!event.isPrimary || event.button !== 0 || send.disabled) return;
        cancel();
        held = false;
        start = [event.clientX, event.clientY];
        timer = setTimeout(show, 500);
      });
      send.addEventListener("pointermove", (event) => {
        if (start && Math.hypot(event.clientX - start[0], event.clientY - start[1]) > 10) cancel();
      });
      for (const type of ["pointerup", "pointercancel", "pointerleave"]) send.addEventListener(type, cancel);
      send.addEventListener("contextmenu", (event) => {
        event.preventDefault();
        cancel();
        show();
      });
      send.addEventListener("click", (event) => {
        if (!held) return;
        held = false;
        event.preventDefault();
        event.stopImmediatePropagation();
      }, true);
      menu.addEventListener("toggle", () => {
        send.setAttribute("aria-expanded", String(menu.matches(":popover-open")));
      });
      document.addEventListener("pointerdown", (event) => {
        if (!menu.contains(event.target) && event.target !== send) menu.hidePopover();
      }, true);
      document.addEventListener("keydown", (event) => {
        if (event.key !== "Escape" || !menu.matches(":popover-open")) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        menu.hidePopover();
      }, true);
    }
    hostView = document.getElementById("host-view");
    lock = document.getElementById("sidebar-lock");
    sidebar = document.getElementById("sidebar");
    if (!hostView || !lock || !sidebar) return;
    const style = document.createElement("style");
    style.textContent = `
      #sidebar-toggle, #sidebar-toggle:hover, #sidebar-toggle:focus-visible { width: 2px; color: transparent; }
      #host-view { padding-left: 222px; }
      #host-view.sidebar-hidden { padding-left: 2px !important; }
      #sidebar { left: 2px; }
      #sidebar, #sidebar h2, #sidebar .sidebar-brand strong { font-size: 16px; }
      #sidebar #identity, #sidebar #host-name-button { font-size: 14px; }
      #sidebar button.workspace, #sidebar button.project { min-height: 44px; }
      #sidebar .workspace-remove, #sidebar .project-pin {
        width: 34px; height: 44px; opacity: 1; pointer-events: auto;
      }
      #sidebar-lock { display: none; }
      .login { width: min(100%, 360px); }
      input, select, textarea, [contenteditable="true"] { font-size: 16px !important; }
    `;
    document.head.append(style);
    for (const id of ["add-workspace", "new-project", "session-menu-button"]) {
      document.getElementById(id).textContent = "+";
    }
    document.querySelectorAll('input[name="username"]').forEach((input) => {
      input.setAttribute("autocapitalize", "none");
      input.setAttribute("autocorrect", "off");
    });
    const toggle = document.getElementById("sidebar-toggle");
    toggle.addEventListener("pointerenter", (event) => event.stopImmediatePropagation(), true);
    sidebar.addEventListener("pointerleave", (event) => event.stopImmediatePropagation(), true);
    sidebar.addEventListener("focusout", (event) => event.stopImmediatePropagation(), true);
    sidebar.addEventListener("click", (event) => {
      if (!docked && event.target.closest("button.project")) open(false);
    });
    toggle.addEventListener("click", (event) => {
      event.stopImmediatePropagation();
      open(!hostView.classList.contains("sidebar-open"));
    }, true);
    document.addEventListener("pointerdown", (event) => {
      if (!docked && !sidebar.contains(event.target)
        && !event.target.closest("#sidebar-toggle")) open(false);
    }, true);
    new MutationObserver(layout).observe(hostView, { attributes: true, attributeFilter: ["hidden"] });
    layout();
  }, { once: true });
})();
