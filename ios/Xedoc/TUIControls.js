(() => {
  const live = document.getElementById("live");
  const composer = document.getElementById("composer");
  if (!live || !composer || !document.getElementById("live-frame")) return;
  document.getElementById("attach").textContent = "+";
  const bar = document.createElement("div");
  bar.id = "mobile-tui-controls";
  bar.setAttribute("role", "group");
  bar.setAttribute("aria-label", "TUI navigation");
  bar.style.cssText = "display:flex;gap:4px;padding:6px 0;border-top:1px solid var(--border)";
  live.style.maxHeight = "max(40px, calc(100dvh - 124px))";
  const input = document.createElement("textarea");
  input.style.cssText = "position:fixed;bottom:0;left:0;width:1px;height:1px;opacity:.01;font-size:16px";
  input.setAttribute("aria-label", "TUI text input");
  input.autocapitalize = "off";
  input.autocomplete = "off";
  input.spellcheck = false;
  input.setAttribute("autocorrect", "off");
  const key = (value) => live.dispatchEvent(new KeyboardEvent("keydown", { key: value, bubbles: true }));
  for (const [label, value] of [
    ["Left", "ArrowLeft"], ["Up", "ArrowUp"], ["Down", "ArrowDown"],
    ["Right", "ArrowRight"], ["Enter", "Enter"], ["Escape", "Escape"], ["Keyboard", null],
  ]) {
    const button = document.createElement("button");
    button.type = "button";
    button.style.cssText = "flex:1;min-width:40px;min-height:44px;background:var(--bg-prompt);font-size:16px";
    button.setAttribute("aria-label", `TUI ${label}`);
    button.textContent = { ArrowLeft: "←", ArrowUp: "↑", ArrowDown: "↓", ArrowRight: "→",
      Escape: "Esc" }[value] || (value || "ABC");
    button.addEventListener("pointerdown", (event) => event.preventDefault());
    button.addEventListener("click", () => {
      if (value) key(value);
      else input.focus({ preventScroll: true });
    });
    bar.append(button);
  }
  bar.append(input);
  live.after(bar);
  let composing = false;
  function paste(text) {
    const data = new DataTransfer();
    data.setData("text/plain", text);
    live.dispatchEvent(new ClipboardEvent("paste", { clipboardData: data, bubbles: true }));
  }
  function flush() {
    if (composing || !input.value) return;
    const text = input.value;
    input.value = "";
    paste(text);
  }
  input.addEventListener("compositionstart", () => { composing = true; });
  input.addEventListener("compositionend", () => { composing = false; flush(); });
  input.addEventListener("input", flush);
  input.addEventListener("beforeinput", (event) => {
    if (composing || event.isComposing) return;
    const value = { deleteContentBackward: "Backspace", deleteContentForward: "Delete",
      insertParagraph: "Enter", insertLineBreak: "Enter" }[event.inputType];
    if (value) { event.preventDefault(); key(value); }
    else if (event.inputType === "insertText" && event.data) {
      event.preventDefault();
      paste(event.data);
    }
  });
  input.addEventListener("keydown", (event) => {
    if (!event.isComposing && ["ArrowLeft", "ArrowUp", "ArrowDown", "ArrowRight",
      "Enter", "Escape", "Backspace", "Delete", "Tab"].includes(event.key)) {
      event.preventDefault();
      key(event.key);
    }
  });
  function sync() {
    bar.hidden = live.hidden;
    if (bar.hidden) { input.blur(); input.value = ""; }
  }
  new MutationObserver(sync).observe(live, { attributes: true, attributeFilter: ["hidden"] });
  sync();
})();
