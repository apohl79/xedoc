"use strict";

window.XedocQuestion = class {
  constructor(element, send, editor) {
    this.element = element;
    this.send = send;
    this.editor = editor;
    this.searchId = 0;
    this.searchTimer = null;
    this.composing = false;
    this.state = null;
    this.key = null;
    this.pendingNotes = [];
    this.serverNotes = "";
    element.addEventListener("keydown", (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        if (this.searchToken) this.closeSearch();
        else this.act("cancel");
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
      this.closeSearch();
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
      const acknowledged = this.pendingNotes.indexOf(state.notes);
      if (acknowledged !== -1) this.pendingNotes.splice(0, acknowledged + 1);
      else if (state.notes !== this.serverNotes) this.pendingNotes = [];
      this.serverNotes = state.notes;
      if (!this.composing && !this.pendingNotes.length && this.editor.text(this.notes) !== state.notes) {
        this.editor.render(state.notes, this.editor.selection(this.notes), this.notes);
        this.closeSearch();
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
    this.closeSearch();
    this.composing = false;
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
    heading.className = "prompt-markdown";
    heading.append(window.XedocPromptMarkdown.render(
      state.confirming ? "Submit with unanswered questions?" : state.question));
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
        const label = document.createElement("div");
        label.className = "prompt-markdown";
        label.append(window.XedocPromptMarkdown.render(option.label));
        text.append(label);
        if (option.description) {
          const description = document.createElement("span");
          description.className = "question-description prompt-markdown";
          description.append(window.XedocPromptMarkdown.render(option.description));
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
      this.notes = document.createElement("div");
      this.notes.id = "question-notes";
      this.notes.className = "prompt-markdown";
      this.notes.contentEditable = "true";
      this.notes.setAttribute("role", "textbox");
      this.notes.setAttribute("aria-multiline", "true");
      this.notes.setAttribute("aria-label", state.options.length ? "Notes" : "Your answer");
      this.editor.render(state.notes, null, this.notes);
      this.notes.addEventListener("click", (event) => {
        if (event.target.closest("a")) event.preventDefault();
        this.updateSearch();
      });
      this.notes.addEventListener("compositionstart", () => { this.composing = true; });
      this.notes.addEventListener("compositionend", () => {
        this.composing = false;
        this.changed();
      });
      this.notes.addEventListener("input", () => {
        if (!this.composing) this.changed();
      });
      this.notes.addEventListener("paste", (event) => {
        event.preventDefault();
        this.insert(event.clipboardData?.getData("text/plain") || "");
      });
      for (const action of ["copy", "cut"]) {
        this.notes.addEventListener(action, (event) => {
          const selection = this.editor.selection(this.notes);
          if (!event.clipboardData || !selection || selection[0] === selection[1]) return;
          event.preventDefault();
          event.clipboardData.setData("text/plain",
            this.editor.text(this.notes).slice(Math.min(...selection), Math.max(...selection)));
          if (action === "cut" && this.notes.isContentEditable) this.insert("");
        });
      }
      this.notes.addEventListener("beforeinput", (event) => {
        if (["insertParagraph", "insertLineBreak"].includes(event.inputType) && !event.isComposing) {
          event.preventDefault();
          this.insert("\n");
        }
      });
      window.XedocPromptMarkdown.bindSelectionWrapping(this.notes, this.editor, () => this.changed());
      this.notes.addEventListener("keydown", (event) => {
        if (event.defaultPrevented) return;
        if (event.isComposing || this.composing) return;
        if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey
            && event.key.toLowerCase() === "a") {
          event.preventDefault();
          const range = document.createRange();
          range.selectNodeContents(this.notes);
          const selection = window.getSelection();
          selection?.removeAllRanges();
          selection?.addRange(range);
          return;
        }
        if (this.searchToken && ["ArrowDown", "ArrowUp", "Enter", "Tab"].includes(event.key)
            && !event.shiftKey && (this.searchPending || this.matches.length)) {
          event.preventDefault();
          if (["Enter", "Tab"].includes(event.key)) this.chooseMatch(this.selectedMatch, event.key === "Tab");
          else if (this.matches.length) {
            this.selectedMatch = (this.selectedMatch + (event.key === "ArrowDown" ? 1 : -1)
              + this.matches.length) % this.matches.length;
            this.drawMatches();
          }
          return;
        }
        if ((event.ctrlKey || event.metaKey) && ["b", "i"].includes(event.key.toLowerCase())) {
          event.preventDefault();
          const selection = this.editor.selection(this.notes);
          if (!selection) return;
          const text = this.editor.text(this.notes);
          const start = Math.min(...selection), end = Math.max(...selection);
          const delimiter = event.key.toLowerCase() === "b" ? "**" : "*";
          this.editor.render(text.slice(0, start) + delimiter + text.slice(start, end)
            + delimiter + text.slice(end), [start + delimiter.length, end + delimiter.length], this.notes);
          this.changed();
          return;
        }
        if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
          event.preventDefault();
          this.closeSearch();
          this.act("next");
        } else if (event.key === "Enter" && event.shiftKey) {
          event.preventDefault();
          this.insert("\n");
        }
      });
      label.append(this.notes);
      this.suggestions = document.createElement("div");
      this.suggestions.className = "question-completions";
      this.suggestions.setAttribute("role", "listbox");
      this.suggestions.setAttribute("aria-label", "Files");
      this.suggestions.id = "question-files";
      this.suggestions.hidden = true;
      this.notes.setAttribute("aria-controls", this.suggestions.id);
      this.notes.setAttribute("aria-autocomplete", "list");
      this.notes.addEventListener("keyup", (event) => {
        if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) this.updateSearch();
      });
      this.notes.addEventListener("blur", () => this.closeSearch());
      this.element.append(options, label, this.suggestions);
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

  changed() {
    const text = this.editor.text(this.notes);
    if (new TextEncoder().encode(text).length > 1_000_000) {
      this.editor.render(this.pendingNotes.at(-1) ?? this.state.notes, null, this.notes);
      return;
    }
    this.editor.render(text, this.editor.selection(this.notes), this.notes);
    this.pendingNotes.push(text);
    if (this.pendingNotes.length > 32) this.pendingNotes.shift();
    this.act("notes", { text });
    this.resizeNotes();
    this.updateSearch();
  }

  insert(text) {
    const source = this.editor.text(this.notes);
    const selection = this.editor.selection(this.notes) || [source.length, source.length];
    const start = Math.min(...selection), end = Math.max(...selection);
    this.editor.render(source.slice(0, start) + text + source.slice(end),
      [start + text.length, start + text.length], this.notes);
    this.changed();
  }

  closeSearch() {
    clearTimeout(this.searchTimer);
    this.searchId++;
    this.searchToken = null;
    this.searchPending = false;
    this.matches = [];
    if (this.suggestions) this.suggestions.hidden = true;
    this.notes?.removeAttribute("aria-activedescendant");
    this.notes?.setAttribute("aria-expanded", "false");
  }

  updateSearch() {
    const selection = this.editor.selection(this.notes);
    const source = this.editor.text(this.notes);
    const match = selection && selection[0] === selection[1]
      && /(?:^|\s)@([^\s@]{0,256})$/.exec(source.slice(0, selection[0]));
    this.closeSearch();
    if (!match || !this.notes.isContentEditable) return;
    this.searchToken = { start: selection[0] - match[1].length - 1, end: selection[0] };
    this.searchPending = true;
    const requestId = this.searchId;
    this.searchTimer = setTimeout(() => {
      this.send({ type: "fileSearch", query: match[1], requestId });
    }, 150);
  }

  searchResults(result) {
    if (!this.searchToken || result.requestId !== this.searchId) return;
    this.searchPending = false;
    this.matches = result.matches.slice(0, 20);
    this.selectedMatch = 0;
    this.drawMatches();
  }

  drawMatches() {
    this.suggestions.replaceChildren();
    this.suggestions.hidden = !this.matches.length;
    this.notes.setAttribute("aria-expanded", String(!!this.matches.length));
    this.matches.forEach((match, index) => {
      const button = document.createElement("button");
      button.type = "button";
      button.id = `question-file-${index}`;
      button.setAttribute("role", "option");
      button.setAttribute("aria-selected", String(index === this.selectedMatch));
      button.textContent = match.path;
      button.addEventListener("pointerdown", (event) => event.preventDefault());
      button.addEventListener("click", () => this.chooseMatch(index));
      this.suggestions.append(button);
    });
    if (this.matches.length) {
      this.notes.setAttribute("aria-activedescendant", `question-file-${this.selectedMatch}`);
      this.suggestions.children[this.selectedMatch].scrollIntoView({ block: "nearest" });
    }
  }

  chooseMatch(index, enterDirectory = false) {
    const match = this.matches[index];
    if (!match || !this.searchToken) return;
    const { start, end } = this.searchToken;
    this.notes.focus({ preventScroll: true });
    this.editor.render(this.editor.text(this.notes), [start, end], this.notes);
    if (enterDirectory && match.match_type === "directory") {
      this.insert(`@${match.path.replace(/[\\/]?$/, "/")}`);
    } else {
      const path = /\s/.test(match.path) && !match.path.includes('"') ? `"${match.path}"` : match.path;
      this.insert(`${path} `);
      this.closeSearch();
    }
  }

  setEnabled(enabled) {
    for (const control of this.element.querySelectorAll("button, textarea")) control.disabled = !enabled;
    if (this.notes) this.notes.contentEditable = String(enabled);
    if (!enabled) this.closeSearch();
  }
};
