(() => {
  window.addEventListener("xedoc-activity-notification", (event) => {
    window.webkit.messageHandlers.xedocNotifications.postMessage(event.detail);
  });
})();
