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
      #sidebar-lock { display: none; }
      .login { width: min(100%, 360px); }
      input, select, textarea { font-size: 16px; }
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
