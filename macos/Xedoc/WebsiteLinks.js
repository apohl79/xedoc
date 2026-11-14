(() => {
  document.addEventListener("click", (event) => {
    if (!event.isTrusted || event.button !== 0) return;
    const anchor = event.composedPath().find((node) => node instanceof HTMLAnchorElement);
    if (!anchor || anchor.hasAttribute("download")) return;
    let target;
    try { target = new URL(anchor.href); } catch { return; }
    if (!["http:", "https:"].includes(target.protocol)
        || target.origin === new URL(location.href).origin) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    window.webkit.messageHandlers.xedocWebsite.postMessage({
      url: target.href, external: event.metaKey,
    });
  }, true);
})();
