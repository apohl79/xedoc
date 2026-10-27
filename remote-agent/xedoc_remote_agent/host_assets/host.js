"use strict";
(() => {
  const byId = (id) => document.getElementById(id);
  let state = null;
  let selectedWorkspace = null;
  let selectedProject = null;
  let projectSource = "create";
  let activeThread = null;
  let projectRequest = 0;
  let sessionRequest = 0;
  let projectViewRequest = 0;
  let accountGeneration = 0;
  const sessionFrames = new Map();
  const openedSessions = new Map();
  const projectCache = new Map();
  const sessionCache = new Map();
  const projectActivityLoaded = new Set();
  const projectActivityRefreshAt = new Map();
  const activity = new window.XedocHostActivity();
  let refreshingRestartState = false;
  let sidebarLocked = (() => {
    try {
      const saved = localStorage.getItem("xedoc-host-sidebar-locked-v2");
      return saved === null || saved === "true";
    } catch {
      return true;
    }
  })();

  function markActiveSessionVisited() {
    if (document.visibilityState !== "visible" || !activeThread) return;
    const frame = sessionFrames.get(activeThread);
    if (frame && !frame.hidden) activity.markVisited(activeThread);
  }

  function refreshVisibleActivity() {
    markActiveSessionVisited();
    activity.markVisible();
  }

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
    if (activeThread === threadId) markActiveSessionVisited();
    activity.paint();
  });
  document.addEventListener("visibilitychange", refreshVisibleActivity);
  window.addEventListener("focus", refreshVisibleActivity);

  function projectPins() {
    try {
      const pins = JSON.parse(localStorage.getItem(`xedoc-pins:${state.account.username}:${selectedWorkspace}`));
      return Array.isArray(pins) ? pins.filter((name) => typeof name === "string") : [];
    } catch {
      return [];
    }
  }

  function applyProjectFilter() {
    const query = byId("project-search").value.trim().toLowerCase();
    const rows = [...byId("projects").children].filter((row) =>
      row.querySelector(".project"));
    let visible = 0;
    for (const row of rows) {
      const matches = row.querySelector(".project").dataset.name.toLowerCase().includes(query);
      row.hidden = !matches;
      if (matches) visible++;
    }
    byId("project-search-empty").hidden = !query || rows.length === 0 || visible > 0;
  }

  function setSidebarOpen(open) {
    const hostView = byId("host-view");
    hostView.classList.toggle("sidebar-open", open);
    hostView.classList.toggle("sidebar-hidden", !sidebarLocked);
    byId("sidebar-toggle").setAttribute("aria-expanded", String(open));
  }

  function setSidebarLocked(locked) {
    sidebarLocked = locked;
    try {
      localStorage.setItem("xedoc-host-sidebar-locked-v2", String(locked));
    } catch {
      // Keep the current session usable when browser storage is unavailable.
    }
    const button = byId("sidebar-lock");
    button.setAttribute("aria-pressed", String(locked));
    button.title = locked ? "Allow sidebar to auto-hide" : "Keep sidebar visible";
    button.setAttribute("aria-label", button.title);
    const toggle = byId("sidebar-toggle");
    toggle.title = locked ? "Hide sidebar" : "Keep sidebar visible";
    toggle.setAttribute("aria-label", toggle.title);
    setSidebarOpen(locked);
  }

  byId("sidebar-toggle").addEventListener("pointerenter", () => setSidebarOpen(true));
  byId("sidebar-toggle").addEventListener("click", () => setSidebarLocked(!sidebarLocked));
  byId("sidebar").addEventListener("pointerleave", () => {
    if (!sidebarLocked) setSidebarOpen(false);
  });
  byId("sidebar").addEventListener("focusin", () => setSidebarOpen(true));
  byId("sidebar").addEventListener("focusout", (event) => {
    if (!sidebarLocked && !byId("sidebar").contains(event.relatedTarget)
      && !byId("sidebar").matches(":hover")) setSidebarOpen(false);
  });
  byId("sidebar-lock").addEventListener("click", () => {
    setSidebarLocked(!sidebarLocked);
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

  function projectCacheKey(workspaceId, username = state?.account?.username) {
    return username ? JSON.stringify([username, workspaceId]) : null;
  }

  function projectSessionKey(workspaceId, projectName, username = state?.account?.username) {
    return username ? JSON.stringify([username, workspaceId, projectName]) : null;
  }

  function showLogin() {
    accountGeneration++;
    projectViewRequest++;
    state = null;
    activeThread = null;
    selectedWorkspace = null;
    selectedProject = null;
    projectRequest++;
    sessionRequest++;
    activity.setAccount(null);
    sessionFrames.clear();
    openedSessions.clear();
    projectCache.clear();
    sessionCache.clear();
    projectActivityLoaded.clear();
    projectActivityRefreshAt.clear();
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
    document.title = `Xedoc - ${state.hostname || "Host"}`;
    activity.setAccount(state.account.username);
    byId("login-view").hidden = true;
    byId("host-view").hidden = false;
    setSidebarLocked(sidebarLocked);
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
      const row = text("div", "", "workspace-row");
      const element = button(entry.workspaceId, async () => {
        selectedWorkspace = entry.workspaceId;
        selectedProject = null;
        activeThread = null;
        sessionRequest++;
        await renderWorkspace();
      });
      element.className = `workspace${selectedWorkspace === entry.workspaceId ? " selected" : ""}`;
      element.dataset.id = entry.workspaceId;
      const remove = document.createElement("button");
      remove.type = "button";
      remove.className = "workspace-remove";
      remove.title = `Remove ${entry.workspaceId}`;
      remove.setAttribute("aria-label", remove.title);
      remove.hidden = !admin;
      const icon = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      icon.setAttribute("viewBox", "0 0 20 20");
      icon.setAttribute("fill", "none");
      icon.setAttribute("stroke", "currentColor");
      icon.setAttribute("stroke-width", "1.6");
      icon.setAttribute("stroke-linecap", "round");
      icon.setAttribute("stroke-linejoin", "round");
      icon.setAttribute("aria-hidden", "true");
      const bin = document.createElementNS("http://www.w3.org/2000/svg", "path");
      bin.setAttribute("d", "M3.5 5.5h13M8 5.5V3.8h4v1.7m3.2 0-.8 10.7H5.6L4.8 5.5m3.4 3v4.7m3.6-4.7v4.7");
      icon.append(bin);
      remove.append(icon);
      remove.addEventListener("click", (event) => {
        event.stopPropagation();
        run(async () => {
          if (!confirm(`Remove workspace “${entry.workspaceId}”? Its directories will not be deleted.`)) return;
          await api("workspace/remove", { workspaceId: entry.workspaceId });
          projectCache.clear();
          projectActivityLoaded.clear();
          projectActivityRefreshAt.clear();
          if (selectedWorkspace === entry.workspaceId) {
            selectedWorkspace = null;
            selectedProject = null;
            activeThread = null;
          }
          await refresh();
        });
      });
      row.append(element, remove);
      return row;
    }));
    if (state.restartRequired) {
      notice("Workspace configuration saved. The broker will restart automatically after active turns finish.");
    }
    await renderWorkspace();
  }

  function applyProjectActivity(workspaceId, projects) {
    const key = projectCacheKey(workspaceId);
    if (!key) return;
    projectCache.set(key, projects);
    projectActivityLoaded.add(key);
    projectActivityRefreshAt.set(key, Date.now());
    activity.observeProjects(projects, workspaceId);
    if (workspaceId !== selectedWorkspace) return;
    const rows = new Map([...byId("projects").children].map((row) => {
      const project = row.querySelector(".project");
      return [project?.dataset.name, row];
    }));
    const pins = new Set(projectPins());
    const sorted = [...projects].sort((left, right) =>
      Number(pins.has(right.name)) - Number(pins.has(left.name))
      || (right.updatedAt || 0) - (left.updatedAt || 0)
      || left.name.localeCompare(right.name));
    for (const project of sorted) {
      const row = rows.get(project.name);
      if (row) byId("projects").append(row);
    }
    activity.paint();
  }

  async function refreshProjectActivity(workspaceId, force = false) {
    const username = state?.account?.username;
    const generation = accountGeneration;
    const key = projectCacheKey(workspaceId, username);
    if (!key || (!force && projectActivityLoaded.has(key))) return;
    const result = await api(
      `projects?workspaceId=${encodeURIComponent(workspaceId)}&activity=1`
    );
    if (generation !== accountGeneration || state?.account?.username !== username) return;
    applyProjectActivity(workspaceId, result.projects || []);
  }

  async function refreshRestartState() {
    if (refreshingRestartState) return;
    refreshingRestartState = true;
    const previous = state;
    try {
      state = await api("state");
      const workspaceId = selectedWorkspace;
      const key = projectCacheKey(workspaceId);
      if (
        key
        && projectActivityLoaded.has(key)
        && Date.now() - (projectActivityRefreshAt.get(key) || 0) >= 30_000
      ) {
        await refreshProjectActivity(workspaceId, true);
        if (workspaceId === selectedWorkspace && state) {
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
    projectViewRequest++;
    sessionFrames.forEach((frame) => { frame.hidden = true; });
    const entry = state.workspaces.find((workspace) => workspace.workspaceId === selectedWorkspace);
    byId("host-view").classList.toggle("has-project", Boolean(entry && selectedProject));
    byId("new-project").hidden = !entry;
    byId("project-empty").hidden = Boolean(entry && selectedProject);
    byId("session-workspace").hidden = !entry || !selectedProject;
    document.querySelectorAll(".workspace").forEach((element) => {
      element.classList.toggle("selected", element.dataset.id === selectedWorkspace);
    });
    const request = ++projectRequest;
    const username = state.account.username;
    const generation = accountGeneration;
    const cacheKey = projectCacheKey(selectedWorkspace, username);
    const cachedProjects = projectCache.get(cacheKey);
    if (!entry) {
      byId("projects").replaceChildren();
      applyProjectFilter();
      byId("session-workspace").hidden = true;
      byId("project-empty").textContent = "No workspaces are assigned to this account.";
      byId("project-empty").hidden = false;
      return;
    }
    byId("project-empty").textContent = "Select a project to view its sessions.";
    let workspaceProjects = cachedProjects;
    if (!workspaceProjects) {
      byId("projects").replaceChildren(text("div", "Loading projects…", "loading loading-row"));
      applyProjectFilter();
      byId("project-empty").classList.add("loading");
      byId("project-empty").textContent = "Loading projects…";
      byId("project-empty").hidden = false;
      try {
        const result = await api(
          `projects?workspaceId=${encodeURIComponent(selectedWorkspace)}`
        );
        if (
          request !== projectRequest
          || generation !== accountGeneration
          || state?.account?.username !== username
        ) return;
        workspaceProjects = result.projects || [];
        projectCache.set(cacheKey, workspaceProjects);
      } catch (error) {
        if (request === projectRequest) {
          byId("project-empty").textContent = "Could not load projects.";
        }
        throw error;
      } finally {
        if (
          request === projectRequest
          && generation === accountGeneration
          && state?.account?.username === username
        ) byId("project-empty").classList.remove("loading");
      }
    }
    if (
      request !== projectRequest
      || generation !== accountGeneration
      || state?.account?.username !== username
    ) return;
    byId("project-empty").textContent = "Select a project to view its sessions.";
    const pins = new Set(projectPins());
    const projects = [...workspaceProjects].sort((left, right) =>
      Number(pins.has(right.name)) - Number(pins.has(left.name))
      || (right.updatedAt || 0) - (left.updatedAt || 0)
      || left.name.localeCompare(right.name));
    byId("projects").replaceChildren();
    if (!projects.some((project) => project.name === selectedProject)) {
      selectedProject = projects[0]?.name || null;
      activeThread = null;
    }
    byId("host-view").classList.toggle("has-project", Boolean(selectedProject));
    byId("projects").replaceChildren(...projects.map((project) => {
      const element = button("", async () => {
        if (selectedProject !== project.name) activeThread = null;
        selectedProject = project.name;
        document.querySelectorAll(".project").forEach((item) => {
          item.classList.toggle("selected", item.dataset.name === selectedProject);
        });
        byId("host-view").classList.add("has-project");
        await renderProject(project);
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
    applyProjectFilter();
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
    if (request === projectRequest && !projectActivityLoaded.has(cacheKey)) {
      void refreshProjectActivity(selectedWorkspace).catch(() => {});
    }
  }

  async function renderProject(project) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    const request = ++sessionRequest;
    const viewRequest = ++projectViewRequest;
    const username = state.account.username;
    const generation = accountGeneration;
    const isCurrentView = () => (
      viewRequest === projectViewRequest
      && generation === accountGeneration
      && state?.account?.username === username
      && workspaceId === selectedWorkspace
      && projectName === selectedProject
    );
    if (!project || !workspaceId || !projectName) return;
    byId("project-empty").hidden = true;
    byId("session-workspace").hidden = false;
    byId("project-title").textContent = project.name;
    const key = projectSessionKey(workspaceId, projectName, username);
    const cachedSessions = sessionCache.get(key);
    if (cachedSessions) {
      const threadId = applyProjectSessions(cachedSessions);
      if (threadId) {
        byId("session-view").hidden = false;
        void openSession(threadId).catch((error) => notice(error.message));
      } else {
        byId("session-view").hidden = true;
      }
      api(query("project/sessions", { workspaceId, projectName }))
        .then((response) => {
          if (!isCurrentView()) return;
          sessionCache.set(key, response.sessions || []);
          const previousThread = activeThread;
          const threadId = applyProjectSessions(response.sessions || []);
          if (threadId !== previousThread) {
            if (threadId) {
              byId("session-view").hidden = false;
              void openSession(threadId).catch((error) => notice(error.message));
            } else {
              byId("session-view").hidden = true;
              sessionFrames.forEach((frame) => { frame.hidden = true; });
            }
          }
        })
        .catch(() => {});
      return;
    }
    byId("sessions").replaceChildren();
    byId("session-empty").classList.add("loading");
    byId("session-empty").textContent = "Loading sessions…";
    byId("session-empty").hidden = false;
    byId("session-view").hidden = true;
    try {
      const response = await api(query("project/sessions", {
        workspaceId,
        projectName,
      }));
      if (
        request !== sessionRequest
        || !isCurrentView()
      ) return;
      sessionCache.set(key, response.sessions || []);
      const threadId = applyProjectSessions(response.sessions || []);
      if (threadId) {
        byId("session-view").hidden = false;
        await openSession(threadId);
      } else {
        sessionFrames.forEach((frame) => { frame.hidden = true; });
      }
    } catch (error) {
      if (isCurrentView() && byId("session-empty").classList.contains("loading")) {
        byId("session-empty").textContent = "Could not load sessions.";
        byId("session-empty").hidden = false;
      }
      throw error;
    } finally {
      if (isCurrentView()) byId("session-empty").classList.remove("loading");
    }
  }

  function applyProjectSessions(items) {
    const sessions = projectSessions(items);
    byId("sessions").replaceChildren(...sessions.map((session) => sessionTab(session)));
    byId("session-empty").classList.remove("loading");
    byId("session-empty").textContent = sessions.length
      ? ""
      : "No recent sessions. Start a new session or resume one.";
    byId("session-empty").hidden = sessions.length > 0;
    if (!sessions.some((session) => session.threadId === activeThread)) {
      activeThread = sessions[0]?.threadId || null;
    }
    return activeThread;
  }

  function sessionTab(session) {
    const element = document.createElement("div");
    element.className = "session-tab";
    element.dataset.threadId = session.threadId;
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

  async function openSession(threadId, archived = false) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    if (!workspaceId || !projectName) return;
    const request = ++sessionRequest;
    const cachedFrame = sessionFrames.get(threadId);
    if (cachedFrame && openedSessions.has(threadId)) {
      activeThread = threadId;
      byId("session-view").hidden = false;
      byId("session-loading").hidden = true;
      sessionFrames.forEach((frame) => { frame.hidden = frame !== cachedFrame; });
      document.querySelectorAll(".session-tab").forEach((tab) => {
        tab.setAttribute("aria-selected", String(tab.dataset.threadId === threadId));
      });
      markActiveSessionVisited();
      activity.markVisible();
      return;
    }
    byId("session-view").hidden = false;
    byId("session-loading").hidden = false;
    sessionFrames.forEach((frame) => { frame.hidden = true; });
    try {
      const openedSession = await api("session/open", {
        workspaceId,
        projectName,
        threadId,
        archived,
      });
      if (request !== sessionRequest || workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
      activeThread = threadId;
      openedSessions.set(threadId, { ...openedSession, workspaceId, projectName, threadId });
      if (![...byId("sessions").children].some((tab) => tab.dataset.threadId === threadId)) {
        byId("sessions").prepend(sessionTab({ ...openedSession, threadId }));
      }
      byId("session-empty").hidden = true;
      document.querySelectorAll(".session-tab").forEach((tab) => {
        tab.setAttribute("aria-selected", String(tab.dataset.threadId === threadId));
      });
      const result = await api("session/gui", { workspaceId, projectName, threadId });
      if (request !== sessionRequest || workspaceId !== selectedWorkspace || projectName !== selectedProject) return;
      let frame = sessionFrames.get(threadId);
      if (!frame) {
        frame = document.createElement("iframe");
        frame.className = "session-frame";
        frame.title = openedSession.title || "Xedoc session";
        frame.addEventListener("load", () => {
          if (activeThread === threadId) byId("session-loading").hidden = true;
        });
        sessionFrames.set(threadId, frame);
        byId("session-frames").append(frame);
      }
      if (frame.getAttribute("src") !== result.guiUrl) frame.src = result.guiUrl;
      else byId("session-loading").hidden = true;
      sessionFrames.forEach((item) => { item.hidden = item !== frame; });
      markActiveSessionVisited();
      activity.markVisible();
    } catch (error) {
      if (request === sessionRequest) byId("session-loading").hidden = true;
      throw error;
    }
  }

  async function closeSession(threadId) {
    const workspaceId = selectedWorkspace;
    const projectName = selectedProject;
    sessionRequest++;
    await api("session/close", { workspaceId, projectName, threadId });
    sessionFrames.get(threadId)?.remove();
    sessionFrames.delete(threadId);
    openedSessions.delete(threadId);
    sessionCache.delete(projectSessionKey(workspaceId, projectName));
    if (threadId === activeThread) {
      activeThread = null;
      byId("session-view").hidden = true;
    }
    await renderWorkspace();
  }

  async function startNewSession() {
    byId("session-menu").close();
    byId("session-view").hidden = false;
    byId("session-loading").hidden = false;
    sessionFrames.forEach((frame) => { frame.hidden = true; });
    try {
      const result = await api("session/new", {
        workspaceId: selectedWorkspace,
        projectName: selectedProject,
      });
      const key = projectSessionKey(selectedWorkspace, selectedProject);
      const sessions = sessionCache.get(key) || [];
      if (!sessions.some((session) => session.threadId === result.threadId)) {
        sessions.unshift({ ...result, updatedAt: Math.floor(Date.now() / 1000) });
      }
      sessionCache.set(key, sessions);
      activeThread = result.threadId;
      await openSession(result.threadId);
    } catch (error) {
      byId("session-loading").hidden = true;
      throw error;
    }
  }

  async function showResumePicker() {
    byId("session-menu").close();
    byId("resume-project").textContent = selectedProject;
    byId("resume-sessions").replaceChildren(text("p", "Loading sessions…", "loading"));
    byId("resume-dialog").showModal();
    let response;
    try {
      response = await api(query("project/sessions", {
        workspaceId: selectedWorkspace,
        projectName: selectedProject,
        includeHistory: "1",
      }));
    } catch (error) {
      byId("resume-sessions").replaceChildren(text("p", "Could not load sessions.", "muted"));
      throw error;
    }
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
    projectCache.clear();
    projectActivityLoaded.clear();
    projectActivityRefreshAt.clear();
    await refresh();
  });
  byId("search-projects").addEventListener("click", () => {
    const search = byId("project-search");
    const opening = search.hidden;
    search.hidden = !opening;
    byId("search-projects").setAttribute("aria-expanded", String(opening));
    if (opening) {
      search.focus();
    } else {
      search.value = "";
      applyProjectFilter();
      byId("search-projects").focus();
    }
  });
  byId("project-search").addEventListener("input", applyProjectFilter);
  byId("project-search").addEventListener("keydown", (event) => {
    if (event.key !== "Escape") return;
    event.preventDefault();
    byId("search-projects").click();
  });
  function setProjectSource(source) {
    projectSource = source;
    const cloning = source === "clone";
    const github = source === "github";
    const usingClone = cloning || github;
    document.querySelectorAll("[data-project-source]").forEach((button) => {
      button.setAttribute("aria-pressed", String(button.dataset.projectSource === source));
    });
    byId("project-name-field").hidden = usingClone;
    const name = byId("project-form").elements.namedItem("name");
    name.disabled = usingClone;
    name.required = !usingClone;
    byId("project-clone-fields").hidden = !usingClone;
    const url = byId("project-form").elements.namedItem("url");
    url.disabled = !usingClone;
    url.required = usingClone;
    url.type = github ? "text" : "url";
    url.placeholder = github ? "owner/repository or https://github.com/owner/repository" : "https://github.com/owner/repository.git";
    byId("project-clone-label").textContent = github ? "GitHub repository or URL" : "Public HTTPS repository URL";
    byId("project-clone-help").hidden = !github;
    byId("project-form").elements.namedItem("cloneName").disabled = !usingClone;
    byId("project-submit").textContent = usingClone ? "Clone" : "Create";
  }
  document.querySelectorAll("[data-project-source]").forEach((button) => {
    button.addEventListener("click", () => setProjectSource(button.dataset.projectSource));
  });
  byId("new-project").addEventListener("click", () => {
    if (!selectedWorkspace) return;
    byId("project-form").reset();
    setProjectSource("create");
    byId("project-dialog").showModal();
  });
  form("project-form", async (data) => {
    let name = data.get("name");
    if (projectSource === "clone" || projectSource === "github") {
      const submit = byId("project-submit");
      submit.textContent = "Cloning…";
      try {
        const result = await api("project/clone", {
          workspaceId: selectedWorkspace,
          url: data.get("url"),
          name: data.get("cloneName"),
          source: projectSource === "github" ? "github" : "https",
        });
        name = result.name;
      } finally {
        submit.textContent = "Clone";
      }
    } else {
      await api("project/create", { workspaceId: selectedWorkspace, name });
    }
    byId("project-dialog").close();
    selectedProject = name;
    byId("project-search").value = "";
    applyProjectFilter();
    const cacheKey = projectCacheKey(selectedWorkspace);
    projectCache.delete(cacheKey);
    projectActivityLoaded.delete(cacheKey);
    projectActivityRefreshAt.delete(cacheKey);
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
  }, 10000);
  run(async () => {
    try {
      await refresh();
    } catch (error) {
      if (error.status !== 401) throw error;
      showLogin();
    }
  });
})();
