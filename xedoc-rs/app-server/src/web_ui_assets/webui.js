"use strict";

(() => {
  const history = document.getElementById("history");
  const active = document.getElementById("active");
  const live = document.getElementById("live");
  const composer = document.getElementById("composer");
  const input = document.getElementById("input");
  const connection = document.getElementById("connection");
  const attach = document.getElementById("attach");
  const images = document.getElementById("images");
  const notice = document.getElementById("notice");
  const earlier = document.getElementById("earlier");
  const activity = document.getElementById("activity");
  const entry = document.getElementById("entry");
  const attachments = document.getElementById("attachments");
  const menu = document.getElementById("menu");
  const quoteSelection = document.getElementById("quote-selection");
  let selectedQuote = "";
  let linkPreviewTimeout = null;
  let linkPreviewAnchor = null;
  let linkPreviewElement = null;
  const cells = new Map();
  const htmlById = new Map();
  const imageTypes = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
  const pdfPreviews = new Map();
  const pendingPdfPreviews = [];
  let uploadQueue = Promise.resolve();
  const capability = new URLSearchParams(location.hash.slice(1)).get("capability")
    || sessionStorage.getItem("xedoc-webui-capability");
  let socket;
  let ready = false;
  let composing = false;
  let reconnectDelay = 500;
  let reconnectTimer;
  let noticeTimer;
  let lastLive = null;
  let lastActivity = null;
  let lastPopup = null;
  let popupActive = false;
  let activeThread = null;
  let activeWidth = null;
  let historyBefore = null;
  let historyRequest = null;
  let composerState = null;
  let revision = 0;
  let inFlight = null;
  let dirty = false;
  let submitPending = false;
  const pendingActions = [];
  let menuKind = null;

  function setConnection(text, state) {
    ready = state === "live";
    connection.textContent = text;
    connection.dataset.state = state;
    input.contentEditable = String(ready);
    attach.disabled = !ready;
    document.getElementById("send").disabled = !ready;
    composer.setAttribute("aria-disabled", String(!ready));
    if (!ready) quoteSelection.hidden = true;
  }

  function showNotice(message) {
    clearTimeout(noticeTimer);
    notice.textContent = message;
    notice.hidden = false;
    noticeTimer = setTimeout(() => { notice.hidden = true; }, 6000);
  }

  function send(message) {
    if (!ready || !socket || socket.readyState !== WebSocket.OPEN) return false;
    socket.send(JSON.stringify(message));
    return true;
  }

  function editorText(node = input) {
    const isBlock = (child) => ["DIV", "P", "LI"].includes(child?.nodeName);
    const text = Array.from(node.childNodes).map((child) => {
      if (child.nodeType === Node.TEXT_NODE) return child.textContent;
      if (child.nodeName === "BR") return "\n";
      let text = editorText(child);
      if (isBlock(child)) {
        if (child.lastChild?.nodeName === "BR") text = text.replace(/\n$/, "");
        return `${text}\n`;
      }
      return text;
    }).join("");
    return isBlock(node.lastChild) ? text.replace(/\n$/, "") : text;
  }

  function editorSelection() {
    const selection = window.getSelection();
    if (!selection?.rangeCount || !input.contains(selection.anchorNode)) return null;
    const offset = (node, position) => {
      const range = document.createRange();
      range.selectNodeContents(input);
      range.setEnd(node, position);
      return editorText(range.cloneContents()).length;
    };
    return [offset(selection.anchorNode, selection.anchorOffset),
      offset(selection.focusNode, selection.focusOffset)];
  }

  function restoreSelection(offsets) {
    if (!offsets) return;
    const locate = (offset) => {
      for (const line of input.children) {
        const length = line.textContent.length;
        if (offset <= length) {
          const walker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
          let node;
          while ((node = walker.nextNode())) {
            if (node.parentElement?.classList.contains("syntax-marker") && offset <= node.length) {
              return [line, 1];
            }
            if (offset <= node.length) return [node, offset];
            offset -= node.length;
          }
          return [line, line.childNodes.length];
        }
        offset -= length + 1;
      }
      return [input, input.childNodes.length];
    };
    window.getSelection()?.setBaseAndExtent(...locate(offsets[0]), ...locate(offsets[1]));
  }

  function editorPrefix(line) {
    return line.match(/^(\s*[-*+] +|> +)(?=\S)/)?.[0];
  }

  function renderEditor(text, selection = null) {
    input.replaceChildren();
    if (text) for (const line of text.split("\n")) {
      const element = document.createElement("div");
      element.className = "editor-line";
      const prefix = editorPrefix(line);
      if (prefix) {
        element.classList.add(prefix.trim().startsWith(">") ? "editor-quote" : "editor-list");
        const marker = document.createElement("span");
        marker.className = "syntax-marker";
        marker.contentEditable = "false";
        marker.textContent = prefix;
        element.append(marker, document.createTextNode(line.slice(prefix.length)));
      } else {
        element.textContent = line;
      }
      if (!line) element.append(document.createElement("br"));
      input.append(element);
    }
    restoreSelection(selection);
  }

  function insertText(text) {
    const source = editorText();
    const selection = editorSelection() || [source.length, source.length];
    const start = Math.min(...selection);
    const end = Math.max(...selection);
    renderEditor(source.slice(0, start) + text + source.slice(end),
      [start + text.length, start + text.length]);
    dirty = true;
    flushDraft();
  }

  function flushDraft() {
    if (!composerState || inFlight !== null || !ready || composing) return;
    if (dirty) {
      const text = editorText();
      inFlight = ++revision;
      if (!send({ type: "draft", text, revision: inFlight, baseRevision: composerState.revision })) {
        inFlight = null;
        return;
      }
      dirty = false;
      return;
    }
    while (pendingActions.length) send(pendingActions.shift());
    if (submitPending) {
      submitPending = false;
      send({ type: "key", key: "Enter" });
    }
  }

  function closeMenu() {
    menu.hidden = true;
    if (menuKind) document.getElementById(menuKind).setAttribute("aria-expanded", "false");
    menuKind = null;
  }

  function submit() {
    submitPending = true;
    dirty = editorText() !== composerState?.text;
    flushDraft();
  }

  function openMenu(kind) {
    if (!ready || !composerState) return;
    if (menuKind === kind) { closeMenu(); return; }
    closeMenu();
    menuKind = kind;
    const button = document.getElementById(kind);
    button.setAttribute("aria-expanded", "true");
    menu.replaceChildren();
    const options = kind === "model" ? composerState.models
      : kind === "router" ? composerState.routerOptions
      : (composerState.models.find((item) => item.id === composerState.model)?.efforts || [])
        .map((id) => ({ id, label: id }));
    const list = document.createElement("div");
    const draw = (filter = "") => {
      list.replaceChildren();
      for (const option of options || []) {
        if (!`${option.label} ${option.id}`.toLowerCase().includes(filter.toLowerCase())) continue;
        const item = document.createElement("button");
        item.type = "button";
        item.role = "menuitemradio";
        item.textContent = option.label || option.id;
        item.setAttribute("aria-checked", String(option.id === composerState[kind]));
        item.addEventListener("click", () => {
          if (option.id !== composerState[kind]) {
            pendingActions.push({ type: "settings", [kind]: option.id });
          }
          closeMenu();
          flushDraft();
          input.focus();
        });
        list.append(item);
      }
      if (!list.childElementCount) {
        const empty = document.createElement("div");
        empty.className = "menu-empty";
        empty.textContent = kind === "model" ? "No matching models" : "No options available";
        list.append(empty);
      }
    };
    let search;
    if (kind === "model") {
      search = document.createElement("input");
      search.type = "search";
      search.placeholder = "Search models…";
      search.setAttribute("aria-label", "Search models");
      search.addEventListener("input", () => draw(search.value));
      menu.append(search);
    }
    draw();
    menu.append(list);
    menu.hidden = false;
    const bounds = button.getBoundingClientRect();
    menu.style.left = `${Math.max(12, Math.min(bounds.right - menu.offsetWidth, innerWidth - menu.offsetWidth - 12))}px`;
    menu.style.top = `${Math.max(12, bounds.top - menu.offsetHeight - 8)}px`;
    (search || list.querySelector("button"))?.focus();
  }

  function renderComposer(state) {
    const threadChanged = composerState && composerState.threadId !== activeThread;
    const wasDialog = composerState?.dialog;
    composerState = { ...state, threadId: activeThread };
    revision = Math.max(revision, state.ackRevision || 0);
    if (threadChanged) {
      inFlight = null;
      dirty = false;
      submitPending = false;
      pendingActions.length = 0;
      for (const url of pdfPreviews.values()) URL.revokeObjectURL(url);
      pdfPreviews.clear();
      for (const preview of pendingPdfPreviews) URL.revokeObjectURL(preview.url);
      pendingPdfPreviews.length = 0;
    }
    if (inFlight !== null && state.ackRevision >= inFlight) inFlight = null;
    if (!dirty && inFlight === null && !composing && editorText() !== state.text) {
      renderEditor(state.text, document.activeElement === input ? editorSelection() : null);
    }
    for (const kind of ["model", "effort", "router"]) {
      const button = document.getElementById(kind);
      button.textContent = state[kind] || kind;
      button.disabled = !ready;
    }
    document.getElementById("session-name").textContent = state.sessionName || "";
    document.getElementById("turn-timer").textContent = state.timer ? `${Math.floor(state.timer / 60)}m ${state.timer % 60}s` : "";
    const signature = JSON.stringify(state.attachments);
    if (attachments.dataset.signature !== signature) {
      attachments.dataset.signature = signature;
      attachments.replaceChildren();
      const anchors = new Set((state.attachments || []).map((item) => item.anchor));
      for (const [anchor, url] of pdfPreviews) {
        if (!anchors.has(anchor)) {
          URL.revokeObjectURL(url);
          pdfPreviews.delete(anchor);
        }
      }
      for (const item of state.attachments || []) {
        const figure = document.createElement("figure");
        figure.className = "attachment";
        const image = /^data:image\/(?:png|jpeg|gif|webp);base64,/.test(item.dataUrl || "");
        const pdf = item.mimeType === "application/pdf";
        if (image) {
          const preview = document.createElement("img");
          preview.src = item.dataUrl;
          preview.alt = item.anchor;
          figure.append(preview);
        } else {
          figure.classList.add("document-attachment");
          if (pdf && !pdfPreviews.has(item.anchor)) {
            const index = pendingPdfPreviews.findIndex((preview) => preview.uploadId === item.uploadId);
            if (index !== -1) pdfPreviews.set(item.anchor, pendingPdfPreviews.splice(index, 1)[0].url);
          }
          const pdfUrl = pdfPreviews.get(item.anchor)
            || (/^data:application\/pdf;base64,/.test(item.dataUrl || "") ? item.dataUrl : "");
          if (pdf && pdfUrl) {
            const preview = document.createElement("object");
            preview.type = "application/pdf";
            preview.data = pdfUrl;
            preview.setAttribute("aria-label", item.name || item.anchor);
            preview.textContent = item.name || item.anchor;
            figure.append(preview);
          }
          const filename = document.createElement("div");
          filename.className = "attachment-filename";
          filename.textContent = item.name || item.anchor;
          filename.title = item.name || item.anchor;
          figure.append(filename);
        }
        const caption = document.createElement("figcaption");
        caption.textContent = item.anchor;
        const remove = document.createElement("button");
        remove.type = "button";
        remove.textContent = "×";
        remove.setAttribute("aria-label", `Remove ${item.anchor}`);
        remove.addEventListener("click", () => {
          const text = editorText().replace(item.anchor, "");
          renderEditor(text);
          dirty = true;
          flushDraft();
          input.focus();
        });
        figure.append(caption, remove);
        attachments.append(figure);
      }
    }
    entry.hidden = !!state.dialog;
    if (state.dialog) quoteSelection.hidden = true;
    live.hidden = !state.dialog;
    activity.hidden = !!state.dialog || !activity.textContent.trim();
    if (state.dialog !== wasDialog && wasDialog !== undefined) {
      closeMenu();
      (state.dialog ? live : input).focus({ preventScroll: true });
    }
    flushDraft();
  }

  function render(frame) {
    setConnection("Live", "live");
    history.hidden = !!frame.overlay;
    const following = history.scrollHeight - history.scrollTop - history.clientHeight < 80;
    if (activeThread !== frame.activeThreadId || activeWidth !== frame.width) {
      history.replaceChildren();
      history.append(active);
      cells.clear();
      htmlById.clear();
      activeThread = frame.activeThreadId;
      activeWidth = frame.width;
      historyBefore = null;
    }
    const ordinal = (id) => Number(id.slice(id.lastIndexOf("-") + 1));
    for (const cell of frame.history || []) {
      let element = cells.get(cell.id);
      if (!element) {
        element = document.createElement("div");
        element.className = "history-cell";
        element.dataset.cellId = cell.id;
        cells.set(cell.id, element);
      }
      let stored = htmlById.get(cell.id);
      if (!stored || stored.total !== cell.rowTotal) {
        element.replaceChildren();
        stored = { total: cell.rowTotal, chunks: new Map() };
        htmlById.set(cell.id, stored);
      }
      const existing = stored.chunks.get(cell.rowStart);
      if (!existing || existing.html !== cell.html || existing.end !== cell.rowEnd) {
        for (const [start, chunk] of stored.chunks) {
          if (start < cell.rowEnd && chunk.end > cell.rowStart) {
            chunk.element.remove();
            stored.chunks.delete(start);
          }
        }
        const fragment = document.createElement("div");
        fragment.innerHTML = cell.html;
        stored.chunks.set(cell.rowStart, {
          html: cell.html, end: cell.rowEnd, element: fragment,
        });
        let previousChunk = null;
        for (const [, chunk] of [...stored.chunks].sort(([left], [right]) => left - right)) {
          const expected = previousChunk ? previousChunk.nextSibling : element.firstChild;
          if (expected !== chunk.element) element.insertBefore(chunk.element, expected);
          previousChunk = chunk.element;
        }
      }
    }
    for (const [id, element] of cells) {
      if (ordinal(id) >= frame.historyTotal) {
        element.remove();
        cells.delete(id);
        htmlById.delete(id);
      }
    }
    let previous = null;
    const ordered = [...cells.entries()].sort(([left], [right]) => ordinal(left) - ordinal(right));
    for (const [, element] of ordered) {
      const expected = previous ? previous.nextSibling : history.firstChild;
      if (expected !== element) history.insertBefore(element, expected);
      previous = element;
    }
    historyRequest = null;
    let expectedIndex = frame.historyTotal - 1;
    historyGap: for (const [id] of [...ordered].reverse()) {
      const index = ordinal(id);
      if (index !== expectedIndex) {
        historyRequest = { before: expectedIndex + 1, rowEnd: null };
        break;
      }
      const stored = htmlById.get(id);
      let expectedRow = stored.total;
      for (const [start, chunk] of [...stored.chunks].sort(([left], [right]) => right - left)) {
        if (chunk.end < expectedRow) {
          historyRequest = { before: index + 1, rowEnd: expectedRow };
          break historyGap;
        }
        expectedRow = start;
      }
      if (expectedRow > 0) {
        historyRequest = { before: index + 1, rowEnd: expectedRow };
        break;
      }
      expectedIndex = index - 1;
    }
    if (!historyRequest && expectedIndex >= 0) {
      historyRequest = { before: expectedIndex + 1, rowEnd: null };
    }
    earlier.hidden = historyRequest === null;
    earlier.disabled = false;
    if (historyBefore !== null && frame.historyEnd === historyBefore.before
      && frame.historyRowEnd === historyBefore.rowEnd) {
      historyBefore = null;
      send({ type: "history", before: frame.historyTotal });
    }
    if (lastLive !== frame.live) {
      live.innerHTML = frame.live || "";
      lastLive = frame.live;
    }
    if (lastActivity !== frame.activity || lastPopup !== frame.popup) {
      activity.innerHTML = `${frame.activity || ""}${frame.popup || ""}`;
      lastActivity = frame.activity;
      lastPopup = frame.popup;
    }
    popupActive = !!frame.popup;
    if (popupActive) activity.scrollTop = activity.scrollHeight;
    document.getElementById("status").innerHTML = frame.status || "";
    if (frame.composer) renderComposer(frame.composer);
    const width = Math.max(40, Math.min(400, Math.floor(history.clientWidth / 8)));
    if (width !== previousWidth && send({ type: "resize", width })) previousWidth = width;
    if (frame.title) document.title = `${frame.title} · XEDOC`;
    if (active.innerHTML !== (frame.active || "")) active.innerHTML = frame.active || "";
    history.append(active);
    setConnection("Live", "live");
    if (following) history.scrollTop = history.scrollHeight;
  }

  earlier.addEventListener("click", () => {
    if (!historyRequest) return;
    historyBefore = { ...historyRequest };
    earlier.disabled = true;
    send({ type: "history", ...historyBefore });
  });

  function connect() {
    setConnection("Connecting…", "connecting");
    const protocol = location.protocol === "https:" ? "wss:" : "ws:";
    socket = new WebSocket(`${protocol}//${location.host}/session/${encodeURIComponent(capability)}`);
    socket.addEventListener("message", (event) => {
      let frame;
      try { frame = JSON.parse(event.data); } catch { return; }
      if (frame.type === "frame") {
        reconnectDelay = 500;
        render(frame);
      } else if (frame.type === "disconnected") {
        setConnection("Session disconnected", "offline");
      } else if (frame.type === "error") {
        showNotice(frame.message || "Session input could not be delivered.");
      }
    });
    socket.addEventListener("close", () => {
      if (inFlight !== null) { inFlight = null; dirty = true; }
      setConnection("Disconnected · reconnecting…", "offline");
      reconnectTimer = setTimeout(connect, reconnectDelay);
      reconnectDelay = Math.min(reconnectDelay * 2, 10000);
    });
    socket.addEventListener("error", () => socket.close());
  }

  input.addEventListener("keydown", (event) => {
    if (composing || event.isComposing || event.key === "Process" || event.key === "Dead") return;
    if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey
        && event.key.toLowerCase() === "c") {
      event.preventDefault();
      if (editorText()) {
        renderEditor("", [0, 0]);
        dirty = true;
        flushDraft();
      }
    } else if (popupActive && !event.shiftKey
        && ["ArrowDown", "ArrowUp", "Enter", "Escape", "Tab", "PageUp", "PageDown"].includes(event.key)) {
      event.preventDefault();
      pendingActions.push({ type: "key", key: event.key, ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey });
      dirty = editorText() !== composerState?.text;
      flushDraft();
    } else if (event.key === "Tab" && !event.shiftKey && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      pendingActions.push({ type: "key", key: "Tab" });
      dirty = editorText() !== composerState?.text;
      flushDraft();
    } else if (event.key === "Enter" && event.shiftKey) {
      event.preventDefault();
      insertText("\n");
    } else if (event.key === "Enter" && !event.altKey && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      submit();
    } else if (event.key === "Escape") {
      closeMenu();
      pendingActions.push({ type: "key", key: "Escape" });
      flushDraft();
    }
  });

  input.addEventListener("compositionstart", () => { composing = true; });
  input.addEventListener("beforeinput", (event) => {
    if (!composing && ["insertParagraph", "insertLineBreak"].includes(event.inputType)) {
      event.preventDefault();
      insertText("\n");
    }
  });
  input.addEventListener("compositionend", () => {
    composing = false;
    dirty = true;
    flushDraft();
  });
  input.addEventListener("input", () => {
    if (composing) return;
    const text = editorText();
    const needsFormatting = text.split("\n").length !== input.children.length
      || Array.from(input.children).some((line) => {
      const prefix = editorPrefix(line.textContent);
      return !!prefix !== !!line.querySelector(".syntax-marker");
    });
    if (!text || needsFormatting) renderEditor(text, editorSelection());
    dirty = true;
    flushDraft();
  });
  live.addEventListener("keydown", (event) => {
    if (event.shiftKey && event.key.startsWith("Arrow")) return;
    if (event.metaKey || event.ctrlKey && ["c", "v", "a"].includes(event.key.toLowerCase())) return;
    event.preventDefault();
    send({ type: "key", key: event.key, ctrl: event.ctrlKey, alt: event.altKey, shift: event.shiftKey });
  });
  live.addEventListener("paste", (event) => {
    const text = event.clipboardData?.getData("text/plain");
    if (!text) return;
    event.preventDefault();
    send({ type: "paste", text });
  });
  for (const kind of ["router", "model", "effort"]) {
    document.getElementById(kind).addEventListener("click", () => openMenu(kind));
  }
  document.getElementById("send").addEventListener("click", submit);
  function updateQuoteSelection() {
    const selection = window.getSelection();
    selectedQuote = "";
    quoteSelection.hidden = true;
    if (!ready || composerState?.dialog || !selection?.rangeCount || selection.isCollapsed
        || !history.contains(selection.anchorNode) || !history.contains(selection.focusNode)) return;
    const text = selection.toString().trim();
    if (!text) return;
    const rect = selection.getRangeAt(0).getBoundingClientRect();
    const bounds = history.getBoundingClientRect();
    if (rect.bottom < bounds.top || rect.top > bounds.bottom) return;
    selectedQuote = text;
    quoteSelection.hidden = false;
    const width = quoteSelection.offsetWidth;
    const height = quoteSelection.offsetHeight;
    quoteSelection.style.left = `${Math.max(8, Math.min(rect.left, innerWidth - width - 8))}px`;
    const top = rect.top - height - 8;
    quoteSelection.style.top = `${Math.max(8, Math.min(top >= bounds.top ? top : rect.bottom + 8,
      innerHeight - height - 8))}px`;
  }
  document.addEventListener("selectionchange", updateQuoteSelection);
  history.addEventListener("scroll", () => { quoteSelection.hidden = true; });
  window.addEventListener("resize", () => {
    quoteSelection.hidden = true;
    clearLinkTargetPreview();
  });
  function clearLinkTargetPreview() {
    if (linkPreviewTimeout !== null) clearTimeout(linkPreviewTimeout);
    linkPreviewTimeout = null;
    linkPreviewAnchor = null;
    linkPreviewElement?.remove();
    linkPreviewElement = null;
  }
  history.addEventListener("mouseover", (event) => {
    const anchor = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!anchor || !history.contains(anchor) || anchor === linkPreviewAnchor) return;
    clearLinkTargetPreview();
    linkPreviewAnchor = anchor;
    const { clientX, clientY } = event;
    linkPreviewTimeout = setTimeout(() => {
      linkPreviewTimeout = null;
      if (linkPreviewAnchor !== anchor || !anchor.isConnected) return;
      const preview = document.createElement("div");
      preview.id = "link-target-preview";
      preview.setAttribute("role", "tooltip");
      preview.textContent = anchor.href;
      document.body.append(preview);
      const { width, height } = preview.getBoundingClientRect();
      const left = Math.max(8, Math.min(clientX + 12, innerWidth - width - 8));
      const below = clientY + 18;
      const top = below + height <= innerHeight - 8
        ? below
        : Math.max(8, clientY - height - 12);
      preview.style.left = `${left}px`;
      preview.style.top = `${top}px`;
      linkPreviewElement = preview;
    }, 1000);
  });
  history.addEventListener("mouseout", (event) => {
    const anchor = event.target instanceof Element ? event.target.closest("a[href]") : null;
    if (!anchor || event.relatedTarget instanceof Node && anchor.contains(event.relatedTarget)) return;
    if (anchor === linkPreviewAnchor) clearLinkTargetPreview();
  });
  history.addEventListener("click", clearLinkTargetPreview);
  quoteSelection.addEventListener("pointerdown", (event) => {
    event.preventDefault();
  });
  quoteSelection.addEventListener("click", () => {
    if (!ready || composerState?.dialog || !selectedQuote) return;
    const source = editorText();
    const separator = source ? (source.endsWith("\n\n") ? "" : source.endsWith("\n") ? "\n" : "\n\n") : "";
    const quote = selectedQuote.split("\n").map((line) => `> ${line}`).join("\n");
    const text = `${source}${separator}${quote}`;
    quoteSelection.hidden = true;
    selectedQuote = "";
    input.focus({ preventScroll: true });
    renderEditor(text, [text.length, text.length]);
    input.scrollTop = input.scrollHeight;
    dirty = true;
    flushDraft();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") quoteSelection.hidden = true;
  });
  document.addEventListener("pointerdown", (event) => {
    if (!menu.contains(event.target) && !event.target.closest("#settings")) closeMenu();
  });
  menu.addEventListener("keydown", (event) => {
    if (event.key === "Escape") { event.preventDefault(); closeMenu(); input.focus(); }
    if (["ArrowDown", "ArrowUp"].includes(event.key)) {
      event.preventDefault();
      const items = [...menu.querySelectorAll("input, button")];
      const index = items.indexOf(document.activeElement);
      items[(index + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus();
    }
  });
  let previousWidth;
  new ResizeObserver(() => {
    const width = Math.max(40, Math.min(400, Math.floor(history.clientWidth / 8)));
    if (width !== previousWidth && send({ type: "resize", width })) previousWidth = width;
    closeMenu();
  }).observe(history);

  async function upload(file, destination, threadId) {
    if (!ready || destination !== socket || threadId !== activeThread) return;
    if (file.size > 8 * 1024 * 1024) {
      showNotice("Files must be 8 MiB or smaller.");
      return;
    }
    try {
      const dataUrl = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result);
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      if (destination !== socket || threadId !== activeThread || !ready) {
        showNotice("File not sent: session disconnected.");
      } else {
        if (imageTypes.has(file.type)) {
          pendingActions.push({ type: "image", dataUrl });
        } else {
          const uploadId = crypto.randomUUID();
          if (file.type === "application/pdf" || /\.pdf$/i.test(file.name)) {
            if (pendingPdfPreviews.length >= 32) URL.revokeObjectURL(pendingPdfPreviews.shift().url);
            pendingPdfPreviews.push({ uploadId, url: URL.createObjectURL(file) });
          }
          pendingActions.push({ type: "file", name: file.name, dataUrl, uploadId });
        }
        flushDraft();
      }
    } catch {
      showNotice("The file could not be read.");
    }
  }

  function queueUploads(files) {
    const destination = socket;
    const threadId = activeThread;
    for (const file of files) uploadQueue = uploadQueue.then(() => upload(file, destination, threadId));
  }

  input.addEventListener("paste", (event) => {
    event.preventDefault();
    const files = Array.from(event.clipboardData?.files || []);
    if (files.length) {
      queueUploads(files);
    } else {
      const text = event.clipboardData?.getData("text/plain");
      if (text) insertText(text);
    }
  });
  attach.addEventListener("click", () => images.click());
  images.addEventListener("change", () => {
    queueUploads(images.files);
    images.value = "";
    if (ready) input.focus({ preventScroll: true });
  });
  document.addEventListener("dragover", (event) => {
    if (!Array.from(event.dataTransfer?.types || []).includes("Files")) return;
    event.preventDefault();
    document.body.classList.add("dragging");
  });
  document.addEventListener("dragleave", (event) => {
    if (!event.relatedTarget) document.body.classList.remove("dragging");
  });
  document.addEventListener("drop", (event) => {
    event.preventDefault();
    document.body.classList.remove("dragging");
    queueUploads(event.dataTransfer?.files || []);
  });
  window.addEventListener("pagehide", () => {
    clearTimeout(reconnectTimer);
    socket?.close();
    for (const url of pdfPreviews.values()) URL.revokeObjectURL(url);
    for (const preview of pendingPdfPreviews) URL.revokeObjectURL(preview.url);
  });

  if (!capability) {
    setConnection("No session link", "offline");
    return;
  }
  sessionStorage.setItem("xedoc-webui-capability", capability);
  window.history.replaceState(null, "", location.pathname + location.search);
  connect();
})();
