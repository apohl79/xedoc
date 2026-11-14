"use strict";

window.XedocPromptUndo = class {
  constructor(root, editor) {
    this.root = root;
    this.editor = editor;
    this.undo = [];
    this.redo = [];
    this.reset();
    const remember = () => {
      const selection = editor.selection(root);
      if (selection) {
        if (!selection.every((offset, index) => offset === this.current.selection?.[index])) {
          this.lastType = null;
        }
        this.current = { ...this.current, selection };
      }
    };
    root.addEventListener("keydown", (event) => {
      if (event.isComposing || !root.isContentEditable) return;
      remember();
      this.inputType = null;
      if ((event.metaKey || event.ctrlKey) && !event.altKey && event.key.toLowerCase() === "z") {
        event.preventDefault();
        this.restore(event.shiftKey);
      } else if (event.key.length !== 1 && !["Backspace", "Delete"].includes(event.key)) {
        this.lastType = null;
      }
    }, true);
    root.addEventListener("beforeinput", (event) => {
      if (event.isComposing || !root.isContentEditable) return;
      remember();
      if (["historyUndo", "historyRedo"].includes(event.inputType)) {
        event.preventDefault();
        this.restore(event.inputType === "historyRedo");
      } else {
        this.inputType = event.inputType;
      }
    }, true);
    root.addEventListener("compositionstart", () => {
      remember();
      this.inputType = null;
      this.lastType = null;
    });
    root.addEventListener("pointerdown", () => { this.lastType = null; });
    root.addEventListener("blur", () => { this.lastType = null; });
  }

  reset() {
    this.current = { text: this.editor.text(this.root), selection: this.editor.selection(this.root) };
    this.undo.length = 0;
    this.redo.length = 0;
    this.inputType = null;
    this.lastType = null;
  }

  record(text, selection) {
    if (!this.restoring && this.editor.text(this.root) === this.current.text) {
      this.current = { ...this.current,
        selection: this.editor.selection(this.root) || this.current.selection };
    }
    if (text === this.current.text) return;
    const now = performance.now();
    if (!this.restoring) {
      const grouped = ["insertText", "deleteContentBackward", "deleteContentForward"].includes(this.inputType)
        && this.inputType === this.lastType && now - this.lastEdit < 750;
      if (!grouped) this.undo.push(this.current);
      this.redo.length = 0;
    }
    this.current = { text, selection };
    this.lastType = this.inputType;
    this.lastEdit = now;
    this.inputType = null;
    this.trim();
  }

  trim() {
    let size = [...this.undo, ...this.redo].reduce((sum, item) => sum + item.text.length, 0);
    while (this.undo.length + this.redo.length > 100 || size > 2_000_000) {
      const removed = this.undo.length ? this.undo.shift() : this.redo.shift();
      size -= removed.text.length;
    }
  }

  restore(redo) {
    const source = redo ? this.redo : this.undo;
    const target = redo ? this.undo : this.redo;
    const state = source.pop();
    if (!state) return;
    target.push(this.current);
    this.restoring = true;
    try {
      this.editor.render(state.text, state.selection || [state.text.length, state.text.length], this.root);
      this.root.dispatchEvent(new InputEvent("input", {
        bubbles: true, inputType: redo ? "historyRedo" : "historyUndo",
      }));
    } finally {
      this.restoring = false;
      this.lastType = null;
      this.trim();
    }
  }
};
