"use strict";

window.XedocQuestion = class {
  constructor(element, send) {
    this.element = element;
    this.send = send;
    this.state = null;
    this.key = null;
    this.pendingNotes = [];
    this.serverNotes = "";
    element.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        this.act("cancel");
      }
    });
  }

  act(action, values = {}) {
    if (!this.state) return;
    this.send({
      type: "question", requestId: this.state.requestId,
      index: this.state.index, action, ...values,
    });
  }

  button(label, action, className = "") {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = label;
    button.className = className;
    button.addEventListener("click", () => this.act(action));
    return button;
  }

  render(state) {
    this.state = state;
    this.element.hidden = !state;
    if (!state) {
      this.key = null;
      this.pendingNotes = [];
      this.element.replaceChildren();
      return;
    }
    const key = JSON.stringify([state.requestId, state.index, state.confirming]);
    if (key !== this.key) {
      this.key = key;
      this.pendingNotes = [];
      this.serverNotes = state.notes;
      this.build();
    }
    this.progress.textContent = `Question ${state.index + 1} of ${state.total}`;
    this.countdown.textContent = state.remainingSeconds == null
      ? "" : `Auto-submit in ${state.remainingSeconds}s`;
    if (this.notes) {
      const acknowledged = this.pendingNotes.lastIndexOf(state.notes);
      if (acknowledged !== -1) this.pendingNotes.splice(0, acknowledged + 1);
      else if (state.notes !== this.serverNotes) this.pendingNotes = [];
      this.serverNotes = state.notes;
      if (!this.pendingNotes.length && this.notes.value !== state.notes) {
        const start = this.notes.selectionStart;
        const end = this.notes.selectionEnd;
        this.notes.value = state.notes;
        this.notes.setSelectionRange(start, end);
      }
      this.resizeNotes();
    }
    for (const option of this.element.querySelectorAll(".question-option")) {
      const selected = Number(option.dataset.index) === state.selected;
      option.setAttribute("aria-checked", String(selected));
      option.tabIndex = selected || state.selected == null && option.dataset.index === "0" ? 0 : -1;
    }
  }

  build() {
    const state = this.state;
    this.element.replaceChildren();
    this.notes = null;
    const meta = document.createElement("div");
    meta.className = "question-meta";
    this.progress = document.createElement("span");
    this.countdown = document.createElement("span");
    meta.append(this.progress, this.countdown);
    const heading = document.createElement("h2");
    heading.id = "question-heading";
    heading.textContent = state.confirming ? "Submit with unanswered questions?" : state.question;
    this.element.append(meta, heading);
    const footer = document.createElement("div");
    footer.className = "question-actions";
    if (state.confirming) {
      footer.append(this.button("Go back", "back"), this.button("Submit answers", "confirm", "question-primary"));
    } else {
      const options = document.createElement("div");
      options.className = "question-options";
      options.setAttribute("role", "radiogroup");
      options.setAttribute("aria-labelledby", heading.id);
      state.options.forEach((option, index) => {
        const button = document.createElement("button");
        button.type = "button";
        button.className = "question-option";
        button.dataset.index = index;
        button.setAttribute("role", "radio");
        const marker = document.createElement("span");
        marker.className = "question-radio";
        marker.setAttribute("aria-hidden", "true");
        const text = document.createElement("span");
        const label = document.createElement("strong");
        label.textContent = option.label;
        text.append(label);
        if (option.description) {
          const description = document.createElement("span");
          description.className = "question-description";
          description.textContent = option.description;
          text.append(description);
        }
        button.append(marker, text);
        button.addEventListener("click", () => this.act("select", { option: index }));
        button.addEventListener("keydown", (event) => {
          if (event.key === "Enter" && !event.isComposing) {
            event.preventDefault();
            event.stopPropagation();
            this.act("next");
            return;
          }
          const direction = ["ArrowDown", "ArrowRight"].includes(event.key) ? 1
            : ["ArrowUp", "ArrowLeft"].includes(event.key) ? -1 : 0;
          if (!direction) return;
          event.preventDefault();
          const target = (index + direction + state.options.length) % state.options.length;
          options.children[target].focus();
          this.act("select", { option: target });
        });
        options.append(button);
      });
      const label = document.createElement("label");
      label.className = "question-notes-label";
      label.textContent = state.options.length ? "Notes" : "Your answer";
      this.notes = document.createElement("textarea");
      this.notes.id = "question-notes";
      this.notes.rows = 2;
      this.notes.value = state.notes;
      this.notes.maxLength = 1_000_000;
      this.notes.placeholder = state.options.length ? "Add notes…" : "Type your answer…";
      this.notes.addEventListener("input", () => {
        this.pendingNotes.push(this.notes.value);
        if (this.pendingNotes.length > 32) this.pendingNotes.shift();
        this.act("notes", { text: this.notes.value });
        this.resizeNotes();
      });
      this.notes.addEventListener("keydown", (event) => {
        if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
          event.preventDefault();
          this.act("next");
        }
      });
      label.append(this.notes);
      this.element.append(options, label);
      if (state.index > 0) footer.append(this.button("Previous", "previous"));
      footer.append(this.button(state.index + 1 < state.total ? "Next" : "Submit answers", "next", "question-primary"));
    }
    const cancel = this.button("Cancel", "cancel", "question-cancel");
    footer.prepend(cancel);
    this.element.append(footer);
    const focusTarget = this.element.querySelector(`.question-option[data-index="${state.selected ?? 0}"]`)
      || this.notes || footer.querySelector("button:not(.question-cancel)");
    focusTarget?.focus({ preventScroll: true });
  }

  resizeNotes() {
    this.notes.style.height = "auto";
    this.notes.style.height = `${this.notes.scrollHeight + 2}px`;
  }

  setEnabled(enabled) {
    for (const control of this.element.querySelectorAll("button, textarea")) control.disabled = !enabled;
  }
};
