"use strict";
(() => {
  const byId = (id) => document.getElementById(id);
  let state = null;
  let selectedWorkspace = null;
  let selectedProject = null;
  let activeThread = null;
  let projectRequest = 0;
  let sessionRequest = 0;
  const sessionFrames = new Map();
  const openedSessions = new Map();
  const activity = new window.XedocHostActivity();
  let refreshingRestartState = false;
  let sidebarLocked = false;

  window.addEventListener("message", (event) => {
    if (event.origin !== location.origin || event.data?.type !== "xedoc-session-activity") return;
    const entry = [...sessionFrames].find(([, frame]) => frame.contentWindow === event.source);
    if (!entry) return;
    const [threadId] = entry;
    const session = openedSessions.get(threadId);
    if (!session) return;
    const needsInput = Boolean(event.data.needsInput);
    activity.observe({
      threadId,
      status: event.data.turnActive || needsInput ? "active" : "idle",
      needsInput,
    }, session.workspaceId, session.projectName);
    activity.paint();
  });
  document.addEventListener("visibilitychange", () => activity.markVisible());
  window.addEventListener("focus", () => activity.markVisible());

  function projectPins() {
    try {
      const pins = JSON.parse(localStorage.getItem(`xedoc-pins:${state.account.username}:${selectedWorkspace}`));
      return Array.isArray(pins) ? pins.filter((name) => typeof name === "string") : [];
    } catch {
      return [];
    }
  }

  function setSidebarOpen(open) {
    byId("host-view").classList.toggle("sidebar-open", open);
    byId("sidebar-toggle").setAttribute("aria-expanded", String(open));
  }

  byId("sidebar-toggle").addEventListener("pointerenter", () => setSidebarOpen(true));
  byId("sidebar-toggle").addEventListener("click", () =>
    setSidebarOpen(!byId("host-view").classList.contains("sidebar-open")));
  byId("sidebar").addEventListener("pointerleave", () => {
    if (!sidebarLocked) setSidebarOpen(false);
  });
  byId("sidebar").addEventListener("focusin", () => setSidebarOpen(true));
  byId("sidebar").addEventListener("focusout", (event) => {
    if (!sidebarLocked && !byId("sidebar").contains(event.relatedTarget)
      && !byId("sidebar").matches(":hover")) setSidebarOpen(false);
  });
  byId("sidebar-lock").addEventListener("click", () => {
    sidebarLocked = !sidebarLocked;
    byId("sidebar-lock").setAttribute("aria-pressed", String(sidebarLocked));
    byId("host-view").classList.toggle("sidebar-locked", sidebarLocked);
    setSidebarOpen(sidebarLocked);
  });

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
    selectedWorkspace = null;
    selectedProject = null;
    projectRequest++;
    sessionRequest++;
    activity.setAccount(null);
    sessionFrames.clear();
    openedSessions.clear();
    byId("session-frames").replaceChildren();
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
    activity.setAccount(state.account.username);
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
      const workspaceId = selectedWorkspace;
      if (workspaceId) {
        const result = await api(`projects?workspaceId=${encodeURIComponent(workspaceId)}&activity=1`);
        if (workspaceId === selectedWorkspace && state) {
          activity.observeProjects(result.projects || [], workspaceId);
          activity.markVisible();
        }
      }
      if (previous?.restartRequired && !state.restartRequired) {
        notice("Workspace configuration saved. The broker restarted automatically.");
        window.setTimeout(() => {
          if (state && !state.restartRequired) notice();
        }, 4000);
      }
    } catch {
      // The broker briefly drops the connection while restarting.
    } finally {
      refreshingRestartState = false;
    }
  }

  async function renderWorkspace() {
    sessionFrames.forEach((frame) => { frame.hidden = true; });
    const entry = state.workspaces.find((workspace) => workspace.workspaceId === selectedWorkspace);
    byId("host-view").classList.toggle("has-project", Boolean(entry && selectedProject));
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
    activity.observeProjects(result.projects || [], selectedWorkspace);
    const pins = new Set(projectPins());
    const projects = (result.projects || []).sort((left, right) =>
      Number(pins.has(right.name)) - Number(pins.has(left.name))
      || (right.updatedAt || 0) - (left.updatedAt || 0)
      || left.name.localeCompare(right.name));
    if (!projects.some((project) => project.name === selectedProject)) {
      selectedProject = projects[0]?.name || null;
      activeThread = null;
    }
    byId("host-view").classList.toggle("has-project", Boolean(selectedProject));
    byId("projects").replaceChildren(...projects.map((project) => {
      const element = button("", async () => {
        if (selectedProject !== project.name) activeThread = null;
        selectedProject = project.name;
        await renderWorkspace();
      });
      element.className = `project${selectedProject === project.name ? " selected" : ""}`;
      element.title = project.path;
      element.dataset.name = project.name;
      element.append(text("span", project.name, "project-label"));
      activity.attach(element, selectedWorkspace, project.name);
      const pin = button(pins.has(project.name) ? "★" : "☆", async () => {
        if (pins.has(project.name)) pins.delete(project.name);
        else pins.add(project.name);
        localStorage.setItem(`xedoc-pins:${state.account.username}:${selectedWorkspace}`, JSON.stringify([...pins]));
        await renderWorkspace();
      });
      pin.className = "project-pin";
      pin.title = `${pins.has(project.name) ? "Unpin" : "Pin"} ${project.name}`;
      pin.setAttribute("aria-label", pin.title);
      pin.setAttribute("aria-pressed", String(pins.has(project.name)));
      const row = text("div", "", "project-row");
      row.append(element, pin);
      return row;
    }));
    activity.paint();
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
    const response = await api(query("project/sessions", {
      workspaceId,
      projectName,
    }));
    if (request !== sessionRequest || workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
    const sessions = projectSessions(response.sessions);
    byId("sessions").replaceChildren(...sessions.map((session) => sessionTab(session)));
    byId("session-empty").hidden = sessions.length > 0;
    if (!sessions.some((session) => session.threadId === activeThread)) {
      activeThread = sessions[0]?.threadId || null;
    }
    if (!activeThread) {
      byId("session-view").hidden = true;
      sessionFrames.forEach((frame) => { frame.hidden = true; });
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
    activity.attach(element, selectedWorkspace, selectedProject, session.threadId);
    if (session.status) activity.observe(session, selectedWorkspace, selectedProject);
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

  function projectSessions(sessions = []) {
    const result = [...sessions];
    for (const session of openedSessions.values()) {
      if (session.workspaceId === selectedWorkspace && session.projectName === selectedProject
        && !result.some((item) => item.threadId === session.threadId)) result.push(session);
    }
    return result;
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
    const sessions = projectSessions(response.sessions);
    if (openedSession && !sessions.some((session) => session.threadId === openedSession.threadId)) {
      sessions.unshift(openedSession);
    }
    byId("sessions").replaceChildren(...sessions.map(sessionTab));
    activity.paint();
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
    openedSessions.set(threadId, { ...openedSession, workspaceId, projectName, threadId });
    byId("session-view").hidden = false;
    await refreshTabs({ threadId, title: openedSession.title || threadId });
    const result = await api("session/gui", { workspaceId, projectName, threadId });
    if (request !== sessionRequest || workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
    let frame = sessionFrames.get(threadId);
    if (!frame) {
      frame = document.createElement("iframe");
      frame.className = "session-frame";
      frame.title = openedSession.title || "Xedoc session";
      sessionFrames.set(threadId, frame);
      byId("session-frames").append(frame);
    }
    if (frame.getAttribute("src") !== result.guiUrl) frame.src = result.guiUrl;
    sessionFrames.forEach((item) => { item.hidden = item !== frame; });
    activity.markVisible();
  }

  async function closeSession(threadId) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    await api("session/close", { workspaceId, projectName, threadId });
    sessionFrames.get(threadId)?.remove();
    sessionFrames.delete(threadId);
    openedSessions.delete(threadId);
    if (threadId === activeThread) {
      activeThread = null;
      byId("session-view").hidden = true;
    }
    await renderWorkspace();
  }

  async function startNewSession() {
    byId("session-menu").close();
    const result = await api("session/new", {
      workspaceId: selectedWorkspace,
      projectName: selectedProject,
    });
    activeThread = result.threadId;
    await openSession(result.threadId);
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
      activity.observe(session, selectedWorkspace, selectedProject);
      activity.attach(element, selectedWorkspace, selectedProject, session.threadId);
      element.addEventListener("click", () => run(async () => {
        byId("resume-dialog").close();
        activeThread = session.threadId;
        await openSession(session.threadId, session.archived);
      }));
      return element;
    }));
    activity.paint();
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
    if (state && !byId("host-view").hidden) {
      refreshRestartState();
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
