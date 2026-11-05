(() => {
  const send = (body) => window.webkit.messageHandlers.xedocNotifications.postMessage(body);
  const ready = () => {
    const host = document.getElementById("host-view");
    if (host && !host.hidden && document.visibilityState === "visible") send({ kind: "pushReady" });
  };
  const host = document.getElementById("host-view");
  if (host) new MutationObserver(ready).observe(host, { attributes: true, attributeFilter: ["hidden"] });
  document.addEventListener("visibilitychange", ready);
  const login = document.getElementById("login-view");
  if (login) {
    const hosts = document.createElement("button");
    hosts.type = "button";
    hosts.textContent = "Hosts";
    hosts.setAttribute("aria-label", "Choose another Xedoc host");
    hosts.style.cssText = "position:fixed;top:max(12px,env(safe-area-inset-top));right:12px;z-index:20;padding:8px 12px";
    hosts.addEventListener("click", () => send({ kind: "disconnect" }));
    login.append(hosts);
  }
  document.addEventListener("click", (event) => {
    if (!event.target.closest("#host-name-button, a.host-candidate")) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    send({ kind: "disconnect" });
  }, true);
  window.addEventListener("xedoc-activity-notification", (event) => {
    send(event.detail);
  });
  ready();
})();
