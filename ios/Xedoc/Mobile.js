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
    document.querySelectorAll('input, select, textarea, [contenteditable]').forEach((input) => {
      input.style.setProperty("font-size", "16px", "important");
    });
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
      button, a, input, select, textarea, [contenteditable="true"] { touch-action: manipulation; }
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
