"use strict";

(() => {
  const history = document.getElementById("history");
  const live = document.getElementById("live");
  const composer = document.getElementById("composer");
  const input = document.getElementById("input");
  const connection = document.getElementById("connection");
  const attach = document.getElementById("attach");
  const images = document.getElementById("images");
  const notice = document.getElementById("notice");
  const earlier = document.getElementById("earlier");
  const cells = new Map();
  const htmlById = new Map();
  const imageTypes = new Set(["image/png", "image/jpeg", "image/gif", "image/webp"]);
  const capability = new URLSearchParams(location.hash.slice(1)).get("capability")
    || sessionStorage.getItem("xedoc-webui-capability");
  let socket;
  let ready = false;
  let composing = false;
  let reconnectDelay = 500;
  let reconnectTimer;
  let noticeTimer;
  let lastLive = null;
  let activeThread = null;
  let activeWidth = null;
  let historyBefore = null;
  let historyRequest = null;

  function setConnection(text, state) {
    ready = state === "live";
    connection.textContent = text;
    connection.dataset.state = state;
    input.disabled = !ready;
    attach.disabled = !ready;
    composer.setAttribute("aria-disabled", String(!ready));
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

  function render(frame) {
    setConnection("Live", "live");
    history.hidden = !!frame.overlay;
    const following = history.scrollHeight - history.scrollTop - history.clientHeight < 80;
    if (activeThread !== frame.activeThreadId || activeWidth !== frame.width) {
      history.replaceChildren();
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
    if (frame.title) document.title = `${frame.title} · XEDOC`;
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
      setConnection("Disconnected · reconnecting…", "offline");
      reconnectTimer = setTimeout(connect, reconnectDelay);
      reconnectDelay = Math.min(reconnectDelay * 2, 10000);
    });
    socket.addEventListener("error", () => socket.close());
  }

  composer.addEventListener("pointerup", () => {
    if (ready && !window.getSelection()?.toString()) input.focus({ preventScroll: true });
  });
  composer.addEventListener("focus", () => {
    if (ready) input.focus({ preventScroll: true });
  });

  input.addEventListener("keydown", (event) => {
    if (composing || event.isComposing || event.key === "Process" || event.key === "Dead") return;
    const modifier = event.ctrlKey || event.metaKey;
    const key = event.key.toLowerCase();
    if (event.metaKey && ["v", "l", "r", "t", "w", "n", "+", "-", "=", "0"].includes(key)) return;
    if (event.ctrlKey && key === "v") return;
    if (modifier && key === "c" && window.getSelection()?.toString()) return;
    if (["Control", "Alt", "Shift", "Meta", "CapsLock", "NumLock", "ScrollLock"].includes(event.key)) return;
    event.preventDefault();
    send({ type: "key", key: event.key, ctrl: modifier, alt: event.altKey, shift: event.shiftKey });
  });

  input.addEventListener("compositionstart", () => { composing = true; });
  input.addEventListener("compositionend", () => {
    composing = false;
    if (input.value) send({ type: "paste", text: input.value });
    input.value = "";
  });
  input.addEventListener("input", () => {
    if (composing) return;
    if (input.value) send({ type: "paste", text: input.value });
    input.value = "";
  });

  async function upload(file) {
    if (!ready) return;
    if (!imageTypes.has(file.type)) {
      showNotice("Choose a PNG, JPEG, GIF, or WebP image.");
      return;
    }
    if (file.size > 8 * 1024 * 1024) {
      showNotice("Images must be 8 MiB or smaller.");
      return;
    }
    const destination = socket;
    try {
      const dataUrl = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result);
        reader.onerror = () => reject(reader.error);
        reader.readAsDataURL(file);
      });
      if (destination !== socket || !send({ type: "image", dataUrl })) {
        showNotice("Image not sent: session disconnected.");
      }
    } catch {
      showNotice("The image could not be read.");
    }
  }

  input.addEventListener("paste", (event) => {
    event.preventDefault();
    const files = Array.from(event.clipboardData?.files || []);
    if (files.length) {
      for (const file of files) void upload(file);
    } else {
      const text = event.clipboardData?.getData("text/plain");
      if (text) send({ type: "paste", text });
    }
  });
  attach.addEventListener("click", () => images.click());
  images.addEventListener("change", () => {
    for (const file of images.files) void upload(file);
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
    for (const file of event.dataTransfer?.files || []) void upload(file);
  });
  window.addEventListener("pagehide", () => {
    clearTimeout(reconnectTimer);
    socket?.close();
  });

  if (!capability) {
    setConnection("No session link", "offline");
    return;
  }
  sessionStorage.setItem("xedoc-webui-capability", capability);
  window.history.replaceState(null, "", location.pathname + location.search);
  connect();
})();
