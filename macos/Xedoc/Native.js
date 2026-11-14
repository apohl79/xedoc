(() => {
  const send = (body) => window.webkit.messageHandlers.xedocDesktop.postMessage(body);
  const originalFetch = window.fetch;
  window.fetch = async (...args) => {
    const response = await originalFetch(...args);
    if (response.status === 401 && new URL(response.url).origin === location.origin) {
      send({ kind: "signedOut" });
    }
    return response;
  };
  window.addEventListener("xedoc-host-ready", () => {
    if (location.protocol === "http:") document.getElementById("logout").hidden = true;
    const username = document.getElementById("identity")?.textContent;
    if (username) send({ kind: "authenticated", username });
    const navigation = document.getElementById("navigation");
    if (navigation && !document.getElementById("native-pair-device")) {
      const button = document.createElement("button");
      button.id = "native-pair-device";
      button.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="8" cy="8" r="5"/><path d="m11.5 11.5 9 9m-3-3 3-3m-6 0 3-3"/></svg>';
      button.title = "Pair another device with this host";
      button.setAttribute("aria-label", "Pair device");
      button.addEventListener("click", () => send({ kind: "pairDevice" }));
      navigation.append(button);
    }
  });
  document.addEventListener("DOMContentLoaded", () => {
    const style = document.createElement("style");
    style.textContent = `
      #sidebar .sidebar-brand { padding-left: 76px; }
      #sidebar .sidebar-brand strong { font-size: 0; }
      #sidebar .brand-mark { display: none; }
      #host-view { padding-left: 222px; }
      #sidebar { left: 2px; transform: translateX(calc(-100% - 2px)); }
      #host-view.sidebar-open #sidebar { transform: translateX(0); visibility: visible; }
      #host-view.sidebar-hidden { padding-left: 2px !important; }
      #host-view.sidebar-hidden #session-workspace > .tab-strip { padding-left: 80px; }
      #sidebar-toggle { top: 32px; width: 2px; }
      #notice:not([hidden]) { padding-top: 32px; }
    `;
    document.head.append(style);
    const host = document.getElementById("host-view");
    const tabs = document.querySelector("#session-workspace > .tab-strip");
    const updateTitlebar = () => {
      const children = tabs && [...tabs.children].map((child) => child.getBoundingClientRect().right);
      const dragStart = tabs?.getBoundingClientRect().width
        ? Math.max(80, ...children)
        : Math.max(80, host && !host.hidden ? document.getElementById("sidebar")?.getBoundingClientRect().right ?? 0 : 0);
      send({ kind: "titlebar", dragStart });
    };
    if (tabs) {
      const resize = new ResizeObserver(updateTitlebar);
      [tabs, ...tabs.children].forEach((element) => resize.observe(element));
    }
    window.addEventListener("resize", updateTitlebar);
    updateTitlebar();
    const changed = () => { if (host?.hidden) send({ kind: "signedOut" }); };
    if (host) new MutationObserver(changed).observe(host, {
      attributes: true, attributeFilter: ["hidden"],
    });
  }, { once: true });
  document.addEventListener("click", (event) => {
    const candidate = event.target.closest("a.host-candidate");
    if (!candidate && !event.target.closest("#host-name-button")) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    send(candidate ? { kind: "openHost", url: candidate.href } : { kind: "hosts" });
  }, true);
  window.addEventListener("xedoc-activity-notification", (event) => send(event.detail));
})();
