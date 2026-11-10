"use strict";

window.XedocPromptMarkdown = (() => {
  function protocolUrl(destination) {
    if (!/^[a-z][a-z\d+.-]*:\/\//i.test(destination)) return null;
    try {
      const parsed = new URL(destination);
      if (parsed.hostname && !["javascript:", "vbscript:", "data:", "blob:"].includes(parsed.protocol)) {
        return parsed;
      }
    } catch {}
    return null;
  }

  function indentWidth(text) {
    return [...text].reduce((width, character) => width + (character === "\t" ? 4 - width % 4 : 1), 0);
  }

  function listItem(line) {
    const match = line.match(/^([ \t]*)([-*+]|\d+(?:\.\d+)*[.)])(?:[ \t]+(?:\[[ xX]\](?:[ \t]+|$))?|$)/);
    if (!match) return null;
    return {
      prefix: match[0], indent: match[1], marker: match[2],
      width: indentWidth(match[1]),
      ordered: /^\d/.test(match[2]),
      task: /\[[ xX]\]/.test(match[0]),
    };
  }

  function scan(text, selected = new Set(), unindent = false) {
    const stack = [];
    const edits = [];
    let fence = null;
    let position = 0;
    const lines = text.split("\n").map((source, index) => {
      let line = source;
      let quote = line.match(/^(?: {0,3}> ?)+/)?.[0] || "";
      const depth = (quote.match(/>/g) || []).length;
      if (fence?.depth && depth < fence.depth) fence = null;
      if (fence && !fence.depth) quote = "";
      const body = line.slice(quote.length);
      const fenceMatch = body.match(/^\s*(`{3,}|~{3,})(.*)$/);
      const boundary = fenceMatch && (!fence || fenceMatch[1][0] === fence.marker[0]
        && fenceMatch[1].length >= fence.marker.length && !fenceMatch[2].trim());
      const width = indentWidth(body.match(/^[ \t]*/)[0]);
      const indented = !fence && !boundary && width >= 4 && !stack.some((item) => item.width < width);
      const code = !!fence || !!boundary || indented;
      const opening = !!boundary && !fence;
      if (boundary) fence = fence ? null : { marker: fenceMatch[1], depth };
      let item = code || quote ? null : listItem(line);
      if (item) {
        const originalPrefix = item.prefix;
        const moved = selected.has(index) && (!unindent || item.width > 0);
        if (moved) {
          line = unindent ? line.replace(/^(?: {1,2}|\t)/, "") : `  ${line}`;
          item = listItem(line);
        }
        let sibling = null;
        while (stack.length && stack.at(-1).width >= item.width) {
          const previous = stack.pop();
          if (previous.width === item.width) sibling = previous;
        }
        const parent = stack.at(-1);
        if (item.ordered) {
          const numbers = item.marker.slice(0, -1).split(".").map(Number);
          const ordinal = moved
            ? (sibling?.ordered ? sibling.path.at(-1) : 0) + 1 : numbers.at(-1);
          item.path = !moved && numbers.length > 1 ? numbers
            : [...(parent?.ordered ? parent.path : []), ordinal];
          item.bullet = `${item.path.join(".")}${item.marker.slice(-1)}`;
          if (moved) {
            line = item.indent + item.bullet + line.slice(item.indent.length + item.marker.length);
            item = { ...item, ...listItem(line) };
          }
        }
        if (moved) {
          edits.push({ position, removed: originalPrefix.length, added: item.prefix.length });
        }
        stack.push(item);
      } else if (code || line.trim() && !/^\s/.test(line)) {
        stack.length = 0;
      }
      position += source.length + 1;
      return { text: line, item, code, quote, indented, opening,
        language: opening ? fenceMatch[2].trim() : "", boundary: !!boundary };
    });
    return { lines, edits };
  }

  function marker(text) {
    const element = document.createElement("span");
    element.className = "syntax-marker";
    element.contentEditable = "false";
    element.setAttribute("aria-hidden", "true");
    element.textContent = text;
    return element;
  }

  function link(destination, image) {
    destination = destination.replace(/\\([!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~])/g, "$1");
    let safe = false;
    try {
      safe = !!protocolUrl(destination)
        || ["http:", "https:", "mailto:"].includes(new URL(destination, location.href).protocol);
    } catch {}
    const element = document.createElement(safe && !image ? "a" : "span");
    if (!image) {
      element.className = "editor-link";
      element.title = destination;
    }
    if (safe && !image) {
      element.href = destination;
      element.target = "_blank";
      element.rel = "noopener noreferrer";
    }
    return element;
  }

  function inline(parent, text, references) {
    const pattern = /\\[!"#$%&'()*+,\-./:;<=>?@[\\\]^_`{|}~]|&(?:#\d+|#x[\da-fA-F]+|[a-zA-Z][a-zA-Z\d]+);|(`+)([^\n]*?)\1|!?\[([^\]\n]+)\]\(((?:\\.|[^()\\\n]|\((?:\\.|[^()\\\n])*\))+)\)|(\*\*|__|~~)(?=\S)(.+?)(?<!\\)\5|(\*|_)(?=\S)(.+?)(?<!\\)\7|(?:\$[\w][\w./:-]*|\[(?:Image|Doc) #\d+\])|<([a-zA-Z][a-zA-Z\d+.-]*:\/\/[^<>\s]+|mailto:[^<>\s]+)>|!?\[([^\]\n]+)\](?:\[([^\]\n]*)\])?/g;
    let offset = 0;
    for (const match of text.matchAll(pattern)) {
      parent.append(document.createTextNode(text.slice(offset, match.index)));
      let element;
      if (match[0].startsWith("\\")) {
        element = document.createElement("span");
        element.append(marker("\\"), document.createTextNode(match[0].slice(1)));
      } else if (match[0].startsWith("&")) {
        const decoded = document.createElement("textarea");
        decoded.innerHTML = match[0];
        element = document.createElement("span");
        element.className = "syntax-entity";
        element.contentEditable = "false";
        element.dataset.text = decoded.value;
        element.append(marker(match[0]));
      } else if (match[1]) {
        element = document.createElement("code");
        element.append(marker(match[1]), document.createTextNode(match[2]), marker(match[1]));
      } else if (match[3]) {
        const image = match[0].startsWith("!");
        const destination = match[4].match(/^(?:<([^>]+)>|(\S+?))(?:\s+["'].*["'])?$/);
        element = link(destination?.[1] || destination?.[2] || match[4], image);
        element.append(marker(image ? "![" : "["));
        inline(element, match[3], references);
        element.append(marker(`](${match[4]})`));
      } else if (match[5] || match[7]) {
        const delimiter = match[5] || match[7];
        element = document.createElement(delimiter === "~~" ? "s" : match[5] ? "strong" : "em");
        element.append(marker(delimiter));
        inline(element, match[6] || match[8], references);
        element.append(marker(delimiter));
      } else if (match[9]) {
        element = link(match[9], false);
        const mail = match[9].startsWith("mailto:");
        element.append(marker(mail ? "<mailto:" : "<"),
          document.createTextNode(mail ? match[9].slice(7) : match[9]), marker(">"));
      } else if (match[10]) {
        const reference = references.get((match[11] || match[10]).trim().replace(/\s+/g, " ").toLowerCase());
        if (reference) {
          const image = match[0].startsWith("!");
          element = link(reference, image);
          element.append(marker(image ? "![" : "["));
          inline(element, match[10], references);
          element.append(marker(match[0].slice((image ? 2 : 1) + match[10].length)));
        } else {
          element = document.createTextNode(match[0]);
        }
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

  function render(text) {
    const content = document.createElement("div");
    const rendered = [];
    let group = null;
    const append = (element) => {
      rendered.push(element);
      const role = element.classList.contains("editor-list") ? "list"
        : element.classList.contains("editor-table") || element.dataset.tableSeparator ? "table" : null;
      if (!role) group = null;
      else if (group?.getAttribute("role") !== role) {
        group = document.createElement("div");
        group.setAttribute("role", role);
        content.append(group);
      }
      (group || content).append(element);
    };
    const lines = scan(text).lines;
    const references = new Map();
    for (const line of lines) {
      const definition = !line.code && line.text.match(/^ {0,3}\[([^\]]+)\]:\s*(?:<([^>]+)>|(\S+))(?:\s+.*)?$/);
      if (definition) {
        references.set(definition[1].trim().replace(/\s+/g, " ").toLowerCase(), definition[2] || definition[3]);
        line.definition = true;
      }
    }
    let tableColumns = 0;
    const cells = (line) => line.trim().replace(/^\||\|$/g, "").split(/(?<!\\)\|/);
    for (const [index, line] of lines.entries()) {
      const element = document.createElement(line.code ? "pre" : line.quote ? "blockquote" : "div");
      element.className = "editor-line";
      if (line.quote) {
        element.classList.add("editor-quote");
        const depth = (line.quote.match(/>/g) || []).length;
        element.style.marginLeft = `${(depth - 1) * 1.2}em`;
      }
      if (line.code) {
        element.classList.add("editor-code");
        if (line.boundary) {
          element.classList.add("editor-fence");
          element.dataset.language = line.language;
          if (!line.opening) {
            element.classList.add("editor-metadata");
            element.setAttribute("aria-hidden", "true");
          }
        }
        if (line.quote) element.append(marker(line.quote));
        const body = line.text.slice(line.quote.length);
        if (line.boundary) {
          element.append(marker(body));
        } else if (line.indented) {
          const indent = body.match(/^(?: {4}| {0,3}\t)/)[0];
          element.append(marker(indent), document.createTextNode(body.slice(indent.length)));
        } else {
          element.append(document.createTextNode(body));
        }
      } else {
        const next = lines[index + 1];
        const separator = next && !next.code && next.text.includes("|")
          && cells(next.text).every((cell) => /^\s*:?-+:?\s*$/.test(cell));
        if (separator && line.text.includes("|") && cells(line.text).length === cells(next.text).length) {
          tableColumns = cells(line.text).length;
          line.tableHeader = true;
          next.tableSeparator = true;
        } else if (!line.tableSeparator && (!line.text.includes("|") || cells(line.text).length !== tableColumns)) {
          tableColumns = 0;
        }
        const rule = /^ {0,3}(?:(?:\*\s*){3,}|(?:-\s*){3,}|(?:_\s*){3,})$/.test(line.text);
        const setext = next && !next.code && !next.item && /^ {0,3}(?:=+|-+)\s*$/.test(next.text)
          && line.text.trim() && !/^ {0,3}#{1,6}(?:[ \t]|$)/.test(line.text)
          && !line.item && !line.quote && !line.definition && !line.headingSeparator && !rule && !tableColumns;
        if (setext) next.headingSeparator = true;
        if (line.definition || line.tableSeparator || line.headingSeparator) {
          element.classList.add("editor-metadata");
          element.setAttribute("aria-hidden", "true");
          if (line.tableSeparator) element.dataset.tableSeparator = "true";
          element.append(marker(line.text));
          append(element);
          continue;
        }
        if (tableColumns) {
          element.classList.add("editor-table");
          element.setAttribute("role", "row");
          element.style.setProperty("--table-columns", tableColumns);
          if (line.tableHeader) element.classList.add("editor-table-heading");
          const parts = line.text.split(/((?<!\\)\|)/);
          for (const [partIndex, part] of parts.entries()) {
            if (part === "|") {
              element.append(marker(part));
            } else if (partIndex !== 0 && partIndex !== parts.length - 1 || part.trim()) {
              const cell = document.createElement("span");
              cell.className = "editor-table-cell";
              cell.setAttribute("role", line.tableHeader ? "columnheader" : "cell");
              inline(cell, part, references);
              element.append(cell);
            } else {
              element.append(marker(part));
            }
          }
          append(element);
          continue;
        }
        if (rule) {
          element.classList.add("editor-rule");
          element.setAttribute("role", "separator");
          element.append(marker(line.text));
          append(element);
          continue;
        }
        const quote = line.quote;
        const prefix = line.item?.prefix || quote || line.text.match(/^ {0,3}#{1,6}(?:[ \t]+|$)/)?.[0];
        let body = line.text.slice(prefix?.length || 0);
        let closing = "";
        if (prefix) {
          const heading = prefix.match(/^ {0,3}(#+)/);
          element.classList.add(heading ? "editor-heading" : line.item ? "editor-list" : "editor-quote");
          if (line.item) {
            element.setAttribute("role", "listitem");
            element.setAttribute("aria-level", Math.floor(line.item.width / 2) + 1);
            element.style.marginLeft = `${line.item.width / 2 * 1.2}em`;
            if (line.item.ordered) element.dataset.bullet = line.item.bullet;
            if (line.item.task) element.dataset.bullet = /\[[xX]\]/.test(prefix) ? "☑" : "☐";
            if (element.dataset.bullet) {
              element.style.setProperty("--marker-width", `${element.dataset.bullet.length + 1}ch`);
            }
          }
          if (heading) {
            element.dataset.level = heading[1].length;
            closing = body.match(/(?:^|[ \t]+)#+[ \t]*$/)?.[0] || "";
            if (closing) body = body.slice(0, -closing.length);
          }
          if (quote) {
            const depth = (quote.match(/>/g) || []).length;
            element.style.marginLeft = `${(depth - 1) * 1.2}em`;
            const nested = body.match(/^(?:[-*+]|\d+(?:\.\d+)*[.)])\s+/)?.[0];
            if (nested) {
              element.classList.add("editor-list");
              element.setAttribute("role", "listitem");
              element.setAttribute("aria-level", "1");
              element.dataset.bullet = /^\d/.test(nested) ? nested.trim() : "•";
              element.style.setProperty("--marker-width", `${element.dataset.bullet.length + 1}ch`);
              element.append(marker(prefix + nested));
              body = body.slice(nested.length);
            } else {
              element.append(marker(prefix));
            }
          } else {
            element.append(marker(prefix));
          }
        }
        if (setext) {
          element.classList.add("editor-heading");
          element.dataset.level = next.text.trim().startsWith("=") ? "1" : "2";
          for (let previous = index - 1; previous >= 0 && lines[previous].paragraph; previous--) {
            const heading = rendered[previous];
            heading.classList.add("editor-heading");
            heading.dataset.level = element.dataset.level;
            heading.setAttribute("role", "heading");
            heading.setAttribute("aria-level", element.dataset.level);
            lines[previous].paragraph = false;
          }
        }
        line.paragraph = !prefix && !setext && !!body.trim();
        if (element.classList.contains("editor-heading")) {
          element.setAttribute("role", "heading");
          element.setAttribute("aria-level", element.dataset.level);
        }
        inline(element, body, references);
        if (closing) element.append(marker(closing));
        if (!body) element.append(document.createElement("br"));
      }
      if (line.code && !line.text) element.append(document.createElement("br"));
      append(element);
    }
    return content;
  }

  function indent(text, selection, unindent) {
    const start = Math.min(...selection);
    const end = Math.max(...selection);
    const first = text.slice(0, start).split("\n").length - 1;
    const last = text.slice(0, Math.max(start, end - 1)).split("\n").length - 1;
    const selected = new Set(Array.from({ length: last - first + 1 }, (_, index) => first + index));
    const { lines, edits } = scan(text, selected, unindent);
    const adjust = (offset) => offset + edits.reduce((delta, edit) =>
      delta + (edit.position <= offset
        ? edit.added - Math.min(edit.removed, offset - edit.position) : 0), 0);
    return { text: lines.map((line) => line.text).join("\n"), selection: selection.map(adjust) };
  }

  function bindSelectionWrapping(root, editor, changed) {
    let wrapped = null;
    const clear = () => { wrapped = null; };
    root.addEventListener("input", clear);
    root.addEventListener("pointerdown", clear);
    root.addEventListener("compositionstart", clear);
    root.addEventListener("keydown", (event) => {
      if (event.defaultPrevented || event.isComposing || !root.isContentEditable
          || event.ctrlKey || event.metaKey || event.altKey) {
        clear();
        return;
      }
      if (["Shift", "Control", "Meta", "Alt"].includes(event.key)) return;
      const text = editor.text(root);
      const selection = editor.selection(root);
      const previous = wrapped;
      clear();
      if (!selection) return;
      if (event.key === "Backspace" && previous && text === previous.text
          && selection.every((offset, index) => offset === previous.selection[index])) {
        event.preventDefault();
        editor.render(previous.source, previous.originalSelection, root);
        changed();
      } else if (["Backspace", "Delete"].includes(event.key) && selection[0] !== selection[1]) {
        event.preventDefault();
        const start = Math.min(...selection), end = Math.max(...selection);
        editor.render(text.slice(0, start) + text.slice(end), [start, start], root);
        changed();
      } else if (['"', "'", "`"].includes(event.key) && selection[0] !== selection[1]) {
        event.preventDefault();
        const start = Math.min(...selection), end = Math.max(...selection);
        const result = text.slice(0, start) + event.key + text.slice(start, end)
          + event.key + text.slice(end);
        const selected = selection[0] < selection[1] ? [start, end + 2] : [end + 2, start];
        editor.render(result, selected, root);
        changed();
        wrapped = { text: editor.text(root), selection: editor.selection(root),
          source: text, originalSelection: selection };
      }
    });
  }

  return { render, lines: (text) => scan(text).lines, indent, protocolUrl, bindSelectionWrapping };
})();
