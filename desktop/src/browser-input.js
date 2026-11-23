"use strict";

async function browserInput(panel, action, input) {
  const contents = panel.view.webContents;
  if (!panel.view.getVisible()) throw new Error("Native input requires the website panel visible.");
  if (action === "pointer") {
    const { width, height } = panel.view.getBounds();
    const x = input.x;
    const y = input.y;
    const zoom = contents.getZoomFactor();
    if (!Number.isFinite(x) || !Number.isFinite(y) || x < 0 || y < 0 || x >= width / zoom || y >= height / zoom) {
      throw new Error("Pointer coordinates must be inside the visible panel.");
    }
    const send = (type, buttons) => panel.devtools.command({
      method: "Input.dispatchMouseEvent",
      params: { type, x, y, button: type === "mouseMoved" && !buttons ? "none" : "left", buttons, clickCount: 1 },
    });
    contents.focus();
    if (input.event === "move") await send("mouseMoved", panel.pointerDown ? 1 : 0);
    else if (input.event === "down") { await send("mousePressed", 1); panel.pointerDown = true; }
    else if (input.event === "up") { await send("mouseReleased", 0); panel.pointerDown = false; }
    else if (input.event === "click") {
      await send("mouseMoved", 0);
      await send("mousePressed", 1);
      await send("mouseReleased", 0);
      panel.pointerDown = false;
    } else throw new Error("Unsupported pointer event.");
  } else if (action === "type") {
    const allowed = await contents.executeJavaScript(`(() => {
      let doc = document;
      let node = doc.activeElement;
      while (node?.shadowRoot?.activeElement || node?.tagName === "IFRAME") {
        if (node.shadowRoot?.activeElement) node = node.shadowRoot.activeElement;
        else {
          doc = node.contentDocument;
          if (!doc) return false;
          node = doc.activeElement;
        }
      }
      return !!node && !["password", "file"].includes(node.type)
        && (node.isContentEditable || ["INPUT", "TEXTAREA"].includes(node.tagName));
    })()`);
    if (!allowed) throw new Error("Focus an editable, non-password element in an accessible frame first.");
    if (!panel.view.getVisible() || contents.isDestroyed()) throw new Error("Website panel is no longer visible.");
    contents.focus();
    await contents.insertText(input.text);
  } else if (action === "key") {
    const modifiers = (input.modifiers || []).map((name) => ({ meta: "meta", control: "control", alt: "alt", shift: "shift" })[name]);
    const shifted = { "~": "`", "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0", "_": "-", "+": "=", "{": "[", "}": "]", "|": "\\", ":": ";", '"': "'", "<": ",", ">": ".", "?": "/" };
    if ((shifted[input.key] || input.key.length === 1 && input.key !== input.key.toLowerCase()) && !modifiers.includes("shift")) modifiers.push("shift");
    const keyCode = { ArrowUp: "Up", ArrowDown: "Down", ArrowLeft: "Left", ArrowRight: "Right", " ": "Space" }[input.key] || shifted[input.key] || (input.key.length === 1 ? input.key.toUpperCase() : input.key);
    const character = modifiers.includes("shift")
      ? Object.keys(shifted).find((key) => shifted[key] === input.key) || input.key.toUpperCase() : input.key;
    contents.focus();
    contents.setIgnoreMenuShortcuts(true);
    try {
      contents.sendInputEvent({ type: "keyDown", keyCode, modifiers });
      if (input.key.length === 1 && !modifiers.some((name) => ["meta", "control", "alt"].includes(name))) {
        contents.sendInputEvent({ type: "char", keyCode: character, modifiers });
      }
      contents.sendInputEvent({ type: "keyUp", keyCode, modifiers });
    } finally {
      contents.setIgnoreMenuShortcuts(false);
    }
  } else throw new Error("Unsupported native input action.");
  return { delivered: true };
}

module.exports = { browserInput };
