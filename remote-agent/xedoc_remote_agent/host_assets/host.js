"use strict";
(() => {
  const byId = (id) => document.getElementById(id);
  let state = null;
  let selectedWorkspace = null;
  let selectedProject = null;
  let activeThread = null;
  let projectRequest = 0;
  let sessionRequest = 0;
  let latestTurns = [];
  let olderTurns = [];
  let olderCursor = null;
  let transcriptKey = "";
  let loadingSession = false;
  let refreshingRestartState = false;

  function notice(message = "") {
    byId("notice").textContent = message;
    byId("notice").hidden = !message;
  }

  async function api(path, body) {
    const response = await fetch(`/api/${path}`, body === undefined ? {} : {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const result = await response.json();
    if (response.status === 401) showLogin();
    if (!response.ok) {
      throw Object.assign(new Error(result.error || "Request failed"), { status: response.status });
    }
    return result;
  }

  function query(path, values) {
    const params = new URLSearchParams(values);
    return `${path}?${params.toString()}`;
  }

  function showLogin() {
    state = null;
    activeThread = null;
    byId("host-view").hidden = true;
    byId("navigation").hidden = true;
    byId("login-view").hidden = false;
    document.querySelectorAll("dialog[open]").forEach((dialog) => dialog.close());
  }

  function text(tag, value, className = "") {
    const element = document.createElement(tag);
    element.textContent = value == null ? "" : String(value);
    element.className = className;
    return element;
  }

  function button(label, action) {
    const element = text("button", label);
    element.type = "button";
    element.addEventListener("click", () => run(action));
    return element;
  }

  async function run(action) {
    try {
      notice();
      await action();
    } catch (error) {
      notice(error.message);
    }
  }

  function row(title, subtitle) {
    const element = text("div", "", "row");
    const content = text("div", "");
    content.append(text("strong", title));
    if (subtitle) content.append(text("small", subtitle));
    element.append(content);
    return element;
  }

  async function refresh() {
    state = await api("state");
    byId("login-view").hidden = true;
    byId("host-view").hidden = false;
    byId("navigation").hidden = false;
    byId("identity").textContent = state.account.username;
    const admin = state.account.role === "admin";
    byId("users-button").hidden = !admin;
    byId("add-workspace").hidden = !admin;
    if (!state.workspaces.some((entry) => entry.workspaceId === selectedWorkspace)) {
      selectedWorkspace = state.workspaces[0]?.workspaceId || null;
      selectedProject = null;
      activeThread = null;
    }
    byId("workspaces").replaceChildren(...state.workspaces.map((entry) => {
      const element = button(entry.workspaceId, async () => {
        selectedWorkspace = entry.workspaceId;
        selectedProject = null;
        activeThread = null;
        await renderWorkspace();
      });
      element.className = `workspace${selectedWorkspace === entry.workspaceId ? " selected" : ""}`;
      element.dataset.id = entry.workspaceId;
      return element;
    }));
    if (state.restartRequired) {
      notice("Workspace configuration saved. The broker will restart automatically after active turns finish.");
    }
    await renderWorkspace();
  }

  async function refreshRestartState() {
    if (refreshingRestartState) return;
    refreshingRestartState = true;
    const previous = state;
    try {
      state = await api("state");
      if (previous?.restartRequired && !state.restartRequired) {
        notice("Workspace configuration saved. The broker restarted automatically.");
        window.setTimeout(() => {
          if (state && !state.restartRequired) notice();
        }, 4000);
        if (activeThread && !byId("host-view").hidden) await loadSession();
      }
    } catch {
      // The broker briefly drops the connection while restarting.
    } finally {
      refreshingRestartState = false;
    }
  }

  async function renderWorkspace() {
    const entry = state.workspaces.find((workspace) => workspace.workspaceId === selectedWorkspace);
    byId("host-view").classList.toggle("has-project", Boolean(entry && selectedProject));
    byId("workspace-heading").hidden = Boolean(
      entry && selectedProject && state.account.role !== "admin",
    );
    byId("workspace-title").textContent = entry?.workspaceId || "Workspaces";
    byId("workspace-root").textContent = entry?.root || "";
    byId("remove-workspace").hidden = !entry || state.account.role !== "admin";
    byId("new-project").hidden = !entry;
    byId("project-empty").hidden = Boolean(entry && selectedProject);
    byId("session-workspace").hidden = !entry || !selectedProject;
    document.querySelectorAll(".workspace").forEach((element) => {
      element.classList.toggle("selected", element.dataset.id === selectedWorkspace);
    });
    byId("projects").replaceChildren();
    const request = ++projectRequest;
    if (!entry) {
      byId("session-workspace").hidden = true;
      byId("project-empty").textContent = "No workspaces are assigned to this account.";
      byId("project-empty").hidden = false;
      return;
    }
    byId("project-empty").textContent = "Select a project to view its sessions.";
    const result = await api(`projects?workspaceId=${encodeURIComponent(selectedWorkspace)}`);
    if (request !== projectRequest) return;
    const projects = result.projects || [];
    if (!projects.some((project) => project.name === selectedProject)) {
      selectedProject = projects[0]?.name || null;
      activeThread = null;
    }
    byId("host-view").classList.toggle("has-project", Boolean(selectedProject));
    byId("projects").replaceChildren(...projects.map((project) => {
      const element = button(project.name, async () => {
        if (selectedProject !== project.name) activeThread = null;
        selectedProject = project.name;
        await renderWorkspace();
      });
      element.className = `project${selectedProject === project.name ? " selected" : ""}`;
      element.title = project.path;
      element.dataset.name = project.name;
      return element;
    }));
    if (!projects.length) {
      byId("projects").append(text("p", "No projects yet.", "muted"));
      byId("project-empty").textContent = "Create a project directory to start a session.";
      byId("project-empty").hidden = false;
      byId("session-workspace").hidden = true;
      return;
    }
    if (selectedProject) {
      const project = projects.find((item) => item.name === selectedProject);
      if (project) await renderProject(project);
    }
  }

  async function renderProject(project) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    const request = ++sessionRequest;
    if (!project || !workspaceId || !projectName) return;
    byId("project-empty").hidden = true;
    byId("session-workspace").hidden = false;
    byId("project-title").textContent = project.name;
    byId("project-path").textContent = project.path;
    const response = await api(query("project/sessions", {
      workspaceId,
      projectName,
    }));
    if (request !== sessionRequest || workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
    const sessions = response.sessions || [];
    byId("sessions").replaceChildren(...sessions.map((session) => sessionTab(session)));
    byId("session-empty").hidden = sessions.length > 0;
    if (!sessions.some((session) => session.threadId === activeThread)) {
      activeThread = sessions[0]?.threadId || null;
    }
    if (!activeThread) {
      byId("session-view").hidden = true;
      byId("load-older").hidden = true;
      latestTurns = [];
      olderTurns = [];
      olderCursor = null;
      transcriptKey = "";
      return;
    }
    byId("session-view").hidden = false;
    await openSession(activeThread);
  }

  function sessionTab(session) {
    const element = document.createElement("div");
    element.className = "session-tab";
    element.setAttribute("role", "tab");
    element.setAttribute("aria-selected", String(session.threadId === activeThread));
    element.tabIndex = 0;
    element.title = session.title || session.threadId;
    const label = text("span", session.title || session.threadId);
    const close = text("button", "×", "close-tab");
    close.type = "button";
    close.setAttribute("aria-label", `Close ${session.title || "session"}`);
    element.append(label, close);
    element.addEventListener("click", () => run(() => openSession(session.threadId)));
    element.addEventListener("keydown", (event) => {
      if (event.target === element && (event.key === "Enter" || event.key === " ")) {
        event.preventDefault();
        run(() => openSession(session.threadId));
      }
    });
    close.addEventListener("click", (event) => {
      event.stopPropagation();
      run(() => closeSession(session.threadId));
    });
    return element;
  }

  async function refreshTabs(openedSession = null) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    if (!workspaceId || !projectName) return;
    const response = await api(query("project/sessions", {
      workspaceId,
      projectName,
    }));
    if (workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
    const sessions = response.sessions || [];
    if (openedSession && !sessions.some((session) => session.threadId === openedSession.threadId)) {
      sessions.unshift(openedSession);
    }
    byId("sessions").replaceChildren(...sessions.map(sessionTab));
    byId("session-empty").hidden = sessions.length > 0;
    if (!sessions.some((session) => session.threadId === activeThread)) {
      activeThread = sessions[0]?.threadId || null;
    }
  }

  async function openSession(threadId, archived = false) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    if (!workspaceId || !projectName) return;
    const request = ++sessionRequest;
    const openedSession = await api("session/open", { workspaceId, projectName, threadId, archived });
    if (request !== sessionRequest || workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
    activeThread = threadId;
    latestTurns = [];
    olderTurns = [];
    olderCursor = null;
    transcriptKey = "";
    byId("session-view").hidden = false;
    await refreshTabs({ threadId, title: openedSession.title || threadId });
    await loadSession();
  }

  async function closeSession(threadId) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    await api("session/close", { workspaceId, projectName, threadId });
    if (threadId === activeThread) {
      activeThread = null;
      latestTurns = [];
      olderTurns = [];
      olderCursor = null;
      transcriptKey = "";
      byId("session-view").hidden = true;
    }
    await renderWorkspace();
  }

  function sessionDataUrl(cursor = "") {
    const values = { workspaceId: selectedWorkspace, projectName: selectedProject, threadId: activeThread };
    if (cursor) values.cursor = cursor;
    return query("session/data", values);
  }

  async function loadSession({ older = false } = {}) {
    if (!activeThread || loadingSession) return;
    loadingSession = true;
    const currentThread = activeThread;
    try {
      const response = await api(sessionDataUrl(older ? olderCursor : ""));
      if (currentThread !== activeThread) return;
      if (older) {
        const known = new Set(olderTurns.map((turn) => turn.id));
        olderTurns = [...response.turns.filter((turn) => !known.has(turn.id)), ...olderTurns];
        olderCursor = response.nextCursor || null;
      } else {
        latestTurns = response.turns || [];
        if (!olderTurns.length) olderCursor = response.nextCursor || null;
      }
      byId("session-status").textContent = `Session ${response.thread.status}`;
      byId("load-older").hidden = !olderCursor;
      const turns = [...olderTurns, ...latestTurns];
      const key = JSON.stringify(turns);
      if (key !== transcriptKey) {
        transcriptKey = key;
        renderTranscript(turns, older);
      }
    } finally {
      loadingSession = false;
    }
  }

  function renderTranscript(turns, preserveOlderScroll = false) {
    const transcript = byId("transcript");
    const wasNearBottom = transcript.scrollHeight - transcript.scrollTop - transcript.clientHeight < 80;
    const previousHeight = transcript.scrollHeight;
    const articles = turns.map((turn) => {
      const article = text("article", "", "turn");
      for (const item of turn.items || []) {
        if (item.type === "userMessage") {
          article.append(renderMarkdown(item.text, "message user"));
        } else if (item.type === "agentMessage") {
          article.append(renderMarkdown(item.text, "message assistant"));
        } else if (item.type === "tool") {
          const tool = text("div", "", "tool-message");
          tool.append(text("strong", item.label));
          if (item.text) tool.append(text("pre", item.text));
          if (item.output) tool.append(text("pre", item.output));
          article.append(tool);
        }
      }
      return article;
    });
    transcript.replaceChildren(...articles);
    if (preserveOlderScroll) transcript.scrollTop += transcript.scrollHeight - previousHeight;
    else if (wasNearBottom) transcript.scrollTop = transcript.scrollHeight;
  }

  function appendInlineMarkdown(parent, source) {
    const pattern = /(\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)|`([^`]+)`|\*\*(.+?)\*\*|\*(.+?)\*)/g;
    let offset = 0;
    for (const match of source.matchAll(pattern)) {
      parent.append(document.createTextNode(source.slice(offset, match.index)));
      if (match[2]) {
        const link = text("a", match[2]);
        link.href = match[3];
        link.target = "_blank";
        link.rel = "noreferrer noopener";
        parent.append(link);
      } else if (match[4]) {
        parent.append(text("code", match[4]));
      } else if (match[5]) {
        parent.append(text("strong", match[5]));
      } else if (match[6]) {
        parent.append(text("em", match[6]));
      }
      offset = match.index + match[0].length;
    }
    parent.append(document.createTextNode(source.slice(offset)));
  }

  function renderMarkdown(source, className) {
    const container = text("div", "", className);
    const lines = String(source || "").split("\n");
    let paragraph = [];
    let list = null;
    let listKind = "";
    let codeLines = [];
    let inCode = false;

    function flushParagraph() {
      if (!paragraph.length) return;
      const element = text("p", "");
      appendInlineMarkdown(element, paragraph.join(" "));
      container.append(element);
      paragraph = [];
    }

    function flushList() {
      if (!list) return;
      container.append(list);
      list = null;
      listKind = "";
    }

    function flushCode() {
      const pre = text("pre", "");
      pre.append(text("code", codeLines.join("\n")));
      container.append(pre);
      codeLines = [];
    }

    for (const line of lines) {
      if (/^\s*```/.test(line)) {
        if (inCode) flushCode();
        else {
          flushParagraph();
          flushList();
        }
        inCode = !inCode;
        continue;
      }
      if (inCode) {
        codeLines.push(line);
        continue;
      }
      if (!line.trim()) {
        flushParagraph();
        flushList();
        continue;
      }
      const heading = /^(#{1,6})\s+(.+)$/.exec(line);
      if (heading) {
        flushParagraph();
        flushList();
        const element = text(`h${heading[1].length}`, "");
        appendInlineMarkdown(element, heading[2]);
        container.append(element);
        continue;
      }
      const item = /^\s*([-*+]|\d+[.)])\s+(.+)$/.exec(line);
      if (item) {
        flushParagraph();
        const kind = /^\d/.test(item[1]) ? "ol" : "ul";
        if (listKind !== kind) {
          flushList();
          list = document.createElement(kind);
          listKind = kind;
        }
        const element = text("li", "");
        appendInlineMarkdown(element, item[2]);
        list.append(element);
        continue;
      }
      const quote = /^\s*>\s?(.*)$/.exec(line);
      if (quote) {
        flushParagraph();
        flushList();
        const element = text("blockquote", "");
        appendInlineMarkdown(element, quote[1]);
        container.append(element);
        continue;
      }
      flushList();
      paragraph.push(line.trim());
    }
    if (inCode) flushCode();
    flushParagraph();
    flushList();
    return container;
  }

  async function startNewSession() {
    byId("session-menu").close();
    const result = await api("session/new", {
      workspaceId: selectedWorkspace,
      projectName: selectedProject,
    });
    activeThread = result.threadId;
    await renderWorkspace();
    byId("message-form").elements.message.focus();
  }

  async function showResumePicker() {
    byId("session-menu").close();
    byId("resume-project").textContent = selectedProject;
    byId("resume-sessions").replaceChildren(text("p", "Loading sessions…", "muted"));
    byId("resume-dialog").showModal();
    const response = await api(query("project/sessions", {
      workspaceId: selectedWorkspace,
      projectName: selectedProject,
      includeHistory: "1",
    }));
    const sessions = response.sessions || [];
    byId("resume-sessions").replaceChildren(...sessions.map((session) => {
      const element = document.createElement("button");
      element.type = "button";
      element.append(text("span", session.title || session.threadId), text("small",
        session.updatedAt ? new Date(session.updatedAt * 1000).toLocaleString() : ""));
      element.addEventListener("click", () => run(async () => {
        byId("resume-dialog").close();
        activeThread = session.threadId;
        await openSession(session.threadId, session.archived);
      }));
      return element;
    }));
    if (!sessions.length) byId("resume-sessions").append(text("p", "No previous sessions in this project.", "muted"));
  }

  function form(id, action) {
    byId(id).addEventListener("submit", (event) => {
      event.preventDefault();
      const element = event.currentTarget;
      run(async () => {
        const submit = element.querySelector("button:not([type=button])");
        if (submit) submit.disabled = true;
        try {
          await action(new FormData(element), element);
        } finally {
          if (submit) submit.disabled = false;
        }
      });
    });
  }

  form("login", async (data, element) => {
    await api("login", Object.fromEntries(data));
    element.reset();
    await refresh();
  });
  byId("logout").addEventListener("click", () => run(async () => {
    await api("logout", {});
    showLogin();
  }));
  document.querySelectorAll("[data-close]").forEach((element) => {
    element.addEventListener("click", () => element.closest("dialog").close());
  });
  byId("add-workspace").addEventListener("click", () => {
    byId("workspace-form").reset();
    byId("workspace-dialog").showModal();
  });
  form("workspace-form", async (data) => {
    await api("workspace/add", {
      workspaceId: data.get("workspaceId"),
      root: data.get("root"),
      create: data.has("create"),
    });
    byId("workspace-dialog").close();
    await refresh();
  });
  byId("remove-workspace").addEventListener("click", () => run(async () => {
    if (!confirm(`Remove workspace “${selectedWorkspace}”? Its directories will not be deleted.`)) return;
    await api("workspace/remove", { workspaceId: selectedWorkspace });
    selectedWorkspace = null;
    selectedProject = null;
    activeThread = null;
    await refresh();
  }));
  byId("new-project").addEventListener("click", () => {
    if (!selectedWorkspace) return;
    byId("project-form").reset();
    byId("project-dialog").showModal();
  });
  form("project-form", async (data) => {
    await api("project/create", { workspaceId: selectedWorkspace, name: data.get("name") });
    byId("project-dialog").close();
    selectedProject = data.get("name");
    await renderWorkspace();
  });
  async function browse(path = "") {
    const result = await api(`browse${path ? `?path=${encodeURIComponent(path)}` : ""}`);
    byId("picker-path").value = result.path;
    byId("directories").replaceChildren(
      ...(result.parent ? [button("↑ Parent directory", () => browse(result.parent))] : []),
      ...result.entries.map((entry) => button(`▸ ${entry.name}`, () => browse(entry.path))),
    );
  }
  byId("browse").addEventListener("click", () => run(async () => {
    await browse(byId("workspace-form").elements.root.value);
    byId("picker").showModal();
  }));
  form("picker-form", async () => browse(byId("picker-path").value));
  byId("choose-directory").addEventListener("click", () => {
    byId("workspace-form").elements.root.value = byId("picker-path").value;
    byId("picker").close();
  });
  byId("session-menu-button").addEventListener("click", () => {
    if (!selectedProject) return;
    byId("session-menu-project").textContent = selectedProject;
    byId("session-menu").showModal();
  });
  byId("new-session").addEventListener("click", () => run(startNewSession));
  byId("resume-session").addEventListener("click", () => run(showResumePicker));
  byId("load-older").addEventListener("click", () => run(() => loadSession({ older: true })));
  form("message-form", async (data, element) => {
    await api("session/turn", {
      workspaceId: selectedWorkspace,
      projectName: selectedProject,
      threadId: activeThread,
      message: data.get("message"),
    });
    element.reset();
    await loadSession();
  });
  byId("message-form").elements.message.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      byId("message-form").requestSubmit();
    }
  });

  async function users() {
    const result = await api("users");
    const formElement = byId("user-form");
    byId("users").replaceChildren(...result.users.map((user) => {
      const element = row(user.username, user.role === "admin" ? "Administrator" : user.workspaces.join(", "));
      element.append(button("Edit", () => {
        formElement.elements.username.value = user.username;
        formElement.elements.password.value = "";
        formElement.elements.role.value = user.role;
        formElement.querySelectorAll("[name=workspace]").forEach((input) => {
          input.checked = user.workspaces.includes(input.value);
        });
      }), button("Remove", async () => {
        if (confirm(`Remove user “${user.username}”?`)) {
          await api("user/remove", { username: user.username });
          await refresh();
        }
      }));
      return element;
    }));
    byId("assignments").replaceChildren(text("legend", "Assigned workspaces"), ...state.workspaces.map((workspace) => {
      const label = text("label", "", "check");
      const input = document.createElement("input");
      input.type = "checkbox";
      input.name = "workspace";
      input.value = workspace.workspaceId;
      label.append(input, text("span", workspace.workspaceId));
      return label;
    }));
  }
  byId("users-button").addEventListener("click", () => run(async () => {
    byId("user-form").reset();
    await users();
    byId("users-dialog").showModal();
  }));
  form("user-form", async (data) => {
    await api("user/save", {
      username: data.get("username"),
      password: data.get("password"),
      role: data.get("role"),
      workspaces: data.getAll("workspace"),
    });
    showLogin();
  });

  setInterval(() => {
    if (state?.restartRequired) {
      refreshRestartState();
      if (activeThread && !byId("host-view").hidden) {
        loadSession().catch(() => {});
      }
    } else if (state && activeThread && !byId("host-view").hidden) {
      run(loadSession);
    }
  }, 2000);
  run(async () => {
    try {
      await refresh();
    } catch (error) {
      if (error.status !== 401) throw error;
      showLogin();
    }
  });
})();
