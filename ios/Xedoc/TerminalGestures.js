(() => {
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
