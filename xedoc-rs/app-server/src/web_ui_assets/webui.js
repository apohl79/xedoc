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
  const question = new window.XedocQuestion(document.getElementById("question"), send);
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
  let activeStreamKind = null;
  let activeStreamTail = null;
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
    question.setEnabled(ready);
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
              const marker = node.parentElement;
              const index = Array.prototype.indexOf.call(marker.parentNode.childNodes, marker);
              return [marker.parentNode, index + (offset > 0 || index === 0 ? 1 : 0)];
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
    return line.match(/^(\s*(?:[-*+]|\d+[.)]) +(?:\[[ xX]\] +)?|> +|#{1,6} +)(?=\S)/)?.[0];
  }

  function markdownMarker(text) {
    const marker = document.createElement("span");
    marker.className = "syntax-marker";
    marker.contentEditable = "false";
    marker.textContent = text;
    return marker;
  }

  function renderInlineMarkdown(parent, text) {
    const pattern = /(`+)([^\n]*?)\1|\[([^\]\n]+)\]\(([^)\n]+)\)|(\*\*|__|~~)(?=\S)(.+?)\5|(\*|_)(?=\S)(.+?)\7|(?:\$[\w][\w./:-]*|\[(?:Image|Doc) #\d+\])/g;
    let offset = 0;
    for (const match of text.matchAll(pattern)) {
      parent.append(document.createTextNode(text.slice(offset, match.index)));
      let element;
      if (match[1]) {
        element = document.createElement("code");
        element.append(markdownMarker(match[1]), document.createTextNode(match[2]), markdownMarker(match[1]));
      } else if (match[3]) {
        element = document.createElement("span");
        element.className = "editor-link";
        element.title = match[4];
        element.append(markdownMarker("["), document.createTextNode(match[3]), markdownMarker(`](${match[4]})`));
      } else if (match[5] || match[7]) {
        const delimiter = match[5] || match[7];
        element = document.createElement(delimiter === "~~" ? "s" : match[5] ? "strong" : "em");
        element.append(markdownMarker(delimiter));
        renderInlineMarkdown(element, match[6] || match[8]);
        element.append(markdownMarker(delimiter));
      } else {
        element = document.createElement("span");
        element.className = "editor-reference";
        element.textContent = match[0];
      }
      parent.append(element);
      offset = match.index + match[0].length;
    }
    parent.append(document.createTextNode(text.slice(offset)));
  }

  function renderEditor(text, selection = null) {
    const content = document.createElement("div");
    let fence = null;
    for (const line of text.split("\n")) {
      const element = document.createElement("div");
      element.className = "editor-line";
      const fenceMatch = line.match(/^\s*(`{3,}|~{3,})(.*)$/);
      if (fenceMatch && (!fence || fenceMatch[1][0] === fence[0]
          && fenceMatch[1].length >= fence.length && !fenceMatch[2].trim())) {
        element.classList.add("editor-code", "editor-fence");
        element.textContent = line;
        fence = fence ? null : fenceMatch[1];
        content.append(element);
        continue;
      }
      if (fence) {
        element.classList.add("editor-code");
        element.textContent = line;
        if (!line) element.append(document.createElement("br"));
        content.append(element);
        continue;
      }
      const prefix = editorPrefix(line);
      if (prefix) {
        const heading = prefix.match(/^#+/);
        element.classList.add(heading ? "editor-heading" : prefix.trim().startsWith(">") ? "editor-quote" : "editor-list");
        if (heading) element.dataset.level = heading[0].length;
        if (/^\s*\d/.test(prefix)) element.dataset.bullet = prefix.trim();
        if (/\[[ xX]\]/.test(prefix)) element.dataset.bullet = /\[[xX]\]/.test(prefix) ? "☑" : "☐";
        element.append(markdownMarker(prefix));
      }
      renderInlineMarkdown(element, line.slice(prefix?.length || 0));
      if (!line) element.append(document.createElement("br"));
      content.append(element);
    }
    if (input.innerHTML !== content.innerHTML) input.replaceChildren(...content.childNodes);
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
      const previous = editorText();
      const selection = document.activeElement === input ? editorSelection() : null;
      if (selection) {
        let start = 0;
        while (start < previous.length && start < state.text.length
            && previous[start] === state.text[start]) start++;
        let end = previous.length;
        let nextEnd = state.text.length;
        while (end > start && nextEnd > start && previous[end - 1] === state.text[nextEnd - 1]) {
          end--;
          nextEnd--;
        }
        for (let index = 0; index < selection.length; index++) {
          if (selection[index] >= start) {
            selection[index] = selection[index] <= end ? nextEnd : selection[index] + nextEnd - end;
          }
        }
      }
      renderEditor(state.text, selection);
    }
    for (const kind of ["model", "effort", "router"]) {
      const button = document.getElementById(kind);
      const value = kind === "model" ? state.displayModel ?? state.model
        : kind === "effort" && Object.hasOwn(state, "displayEffort") ? state.displayEffort
        : state[kind];
      button.textContent = value || kind;
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
    live.hidden = !state.dialog || !!question.state;
    activity.hidden = !!state.dialog || !activity.textContent.trim();
    if (state.dialog !== wasDialog) {
      closeMenu();
      if (!question.state) (state.dialog ? live : input).focus({ preventScroll: true });
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
      element.hidden = !!cell.hidden;
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
        if (cell.rowStart === 0) {
          const response = fragment.querySelector(".history-markdown:not(.history-streaming)");
          if (response) {
            const spacer = document.createElement("div");
            spacer.className = "history-response-spacer";
            spacer.setAttribute("aria-hidden", "true");
            response.before(spacer);
          }
        }
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
      const activityLines = [...activity.querySelectorAll(":scope > .line")];
      const workingLine = activityLines.find((line) =>
        /^\s*[•◦]?\s*Working\b/.test(line.textContent));
      if (workingLine) {
        const text = workingLine.textContent;
        const interruptHintEnd = text.indexOf(" to interrupt");
        const hintSearchStart = interruptHintEnd === -1
          ? -1
          : interruptHintEnd + " to interrupt".length;
        const hintStart = hintSearchStart === -1
          ? text.lastIndexOf(" • ctrl+d")
          : text.indexOf(" • ", hintSearchStart);
        if (hintStart !== -1) {
          const closeParen = text.indexOf(")", hintStart);
          const hintEnd = closeParen === -1 ? text.length : closeParen;
          const range = document.createRange();
          const walker = document.createTreeWalker(workingLine, NodeFilter.SHOW_TEXT);
          let offset = 0;
          let startNode;
          let startOffset;
          let endNode;
          let endOffset;
          while (walker.nextNode()) {
            const node = walker.currentNode;
            const nodeEnd = offset + node.length;
            if (!startNode && hintStart >= offset && hintStart <= nodeEnd) {
              startNode = node;
              startOffset = hintStart - offset;
            }
            if (hintEnd >= offset && hintEnd <= nodeEnd) {
              endNode = node;
              endOffset = hintEnd - offset;
              break;
            }
            offset = nodeEnd;
          }
          if (startNode && endNode) {
            range.setStart(startNode, startOffset);
            range.setEnd(endNode, endOffset);
            range.deleteContents();
          }
        }
      }
      for (const line of activityLines) {
        const text = line.textContent;
        const workingMatch = /^\s*[•◦]?\s*Working\b/.exec(text);
        const metadataMatch = /\([^)]*esc to interrupt[^)]*\)/.exec(line.textContent);
        if (!workingMatch && !metadataMatch) continue;
        const metadataRange = document.createRange();
        const textWalker = document.createTreeWalker(line, NodeFilter.SHOW_TEXT);
        let textOffset = 0;
        let metadataStartNode;
        let metadataStartOffset;
        let metadataEndNode;
        let metadataEndOffset;
        const metadataStart = workingMatch
          ? workingMatch.index + workingMatch[0].length
          : metadataMatch.index;
        const metadataEnd = workingMatch ? text.length : metadataStart + metadataMatch[0].length;
        if (metadataStart >= metadataEnd) continue;
        while (textWalker.nextNode()) {
          const textNode = textWalker.currentNode;
          const nodeEnd = textOffset + textNode.length;
          if (!metadataStartNode && metadataStart >= textOffset && metadataStart <= nodeEnd) {
            metadataStartNode = textNode;
            metadataStartOffset = metadataStart - textOffset;
          }
          if (metadataEnd >= textOffset && metadataEnd <= nodeEnd) {
            metadataEndNode = textNode;
            metadataEndOffset = metadataEnd - textOffset;
            break;
          }
          textOffset = nodeEnd;
        }
        if (metadataStartNode && metadataEndNode) {
          metadataRange.setStart(metadataStartNode, metadataStartOffset);
          metadataRange.setEnd(metadataEndNode, metadataEndOffset);
          const metadata = document.createElement("span");
          metadata.className = "activity-meta";
          metadata.append(metadataRange.extractContents());
          metadataRange.insertNode(metadata);
        }
      }
      const taskHeading = activityLines.findIndex((line) =>
        /^\s*•\s*Tasks\s+\d+\/\d+/.test(line.textContent));
      if (taskHeading !== -1) {
        activityLines[taskHeading].classList.add("task-heading");
        for (const line of activityLines.slice(taskHeading + 1)) {
          const text = line.textContent;
          const match = /^\s*(?:└\s*)?([✔□])\s+(.+)$/.exec(text);
          if (!match) {
            const overflow = /^\s*\.\.\. (\d+) more$/.exec(text);
            if (!overflow) break;
            line.classList.add("task-overflow");
            line.textContent = `... ${overflow[1]} more`;
            continue;
          }
          const markerStart = text.indexOf(match[1]);
          const labelStart = markerStart + match[1].length + 1;
          const copyStyledRange = (start, end) => {
            const spans = [];
            let offset = 0;
            for (const span of line.children) {
              const text = span.textContent;
              const from = Math.max(0, start - offset);
              const to = Math.min(text.length, end - offset);
              if (from < to) {
                const copy = span.cloneNode(false);
                copy.textContent = text.slice(from, to);
                spans.push(copy);
              }
              offset += text.length;
            }
            return spans;
          };
          const marker = document.createElement("span");
          marker.className = "task-marker";
          marker.append(...copyStyledRange(markerStart, markerStart + match[1].length));
          const label = document.createElement("span");
          label.className = "task-label activity-meta";
          label.append(...copyStyledRange(labelStart, text.length));
          line.classList.add("task-row");
          line.replaceChildren(marker, label);
        }
      }
      lastActivity = frame.activity;
      lastPopup = frame.popup;
    }
    popupActive = !!frame.popup;
    if (popupActive) activity.scrollTop = activity.scrollHeight;
    document.getElementById("status").innerHTML = frame.status || "";
    question.render(frame.question || null);
    if (frame.composer) renderComposer(frame.composer);
    const width = Math.max(40, Math.min(400, Math.floor(history.clientWidth / 8)));
    if (width !== previousWidth && send({ type: "resize", width })) previousWidth = width;
    if (frame.title) document.title = `${frame.title} · XEDOC`;
    if (frame.stream) {
      const update = frame.stream;
      if (update.reset) {
        active.replaceChildren();
        activeStreamKind = update.kind;
        activeStreamTail = null;
      }
      if (update.kind) {
        activeStreamKind = update.kind;
        for (const html of update.stable || []) {
          const page = document.createElement("div");
          page.innerHTML = html;
          active.insertBefore(page, activeStreamTail);
        }
        if (update.tail !== null) {
          if (!activeStreamTail) {
            activeStreamTail = document.createElement("div");
            active.append(activeStreamTail);
          }
          activeStreamTail.innerHTML = update.tail || "";
        }
      } else {
        activeStreamKind = null;
        activeStreamTail = null;
      }
    }
    if (activeStreamKind === null) {
      const activeHtml = `${frame.active || ""}${frame.toolSummary || ""}`;
      if (active.innerHTML !== activeHtml) active.innerHTML = activeHtml;
    }
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
    if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey
        && ["b", "i"].includes(event.key.toLowerCase())) {
      event.preventDefault();
      const text = editorText();
      const selection = editorSelection() || [text.length, text.length];
      const start = Math.min(...selection);
      const end = Math.max(...selection);
      const delimiter = event.key.toLowerCase() === "b" ? "**" : "*";
      renderEditor(text.slice(0, start) + delimiter + text.slice(start, end) + delimiter + text.slice(end),
        [start + delimiter.length, end + delimiter.length]);
      dirty = true;
      flushDraft();
    } else if (event.ctrlKey && !event.altKey && !event.metaKey && !event.shiftKey
        && event.key.toLowerCase() === "c") {
      event.preventDefault();
      if (editorText()) {
        renderEditor("", [0, 0]);
        dirty = true;
        flushDraft();
      }
    } else if ((popupActive || (event.altKey && !event.ctrlKey && !event.metaKey && event.key === "ArrowUp"))
        && !event.shiftKey
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
    renderEditor(editorText(), editorSelection());
    dirty = true;
    flushDraft();
  });
  input.addEventListener("input", () => {
    if (composing) return;
    const text = editorText();
    renderEditor(text, editorSelection());
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
    const selection = editorSelection();
    const cursor = selection
      ? new TextEncoder().encode(editorText().slice(0, Math.min(...selection))).length
      : undefined;
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
          pendingActions.push({ type: "image", dataUrl, cursor });
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
