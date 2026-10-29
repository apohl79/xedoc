"use strict";
window.XedocHostActivity = class {
  constructor() {
    this.account = null;
    this.sessions = new Map();
    this.saved = {};
    this.lastVisit = Date.now() / 1000;
  }

  setAccount(username) {
    if (this.account === username) return;
    this.account = username;
    this.sessions.clear();
    this.saved = {};
    this.lastVisit = Date.now() / 1000;
    if (!username) return;
    try {
      const value = JSON.parse(localStorage.getItem(`xedoc-activity:${username}`));
      if (value && typeof value === "object" && !Array.isArray(value)) {
        this.saved = Object.fromEntries(Object.entries(value).slice(-1000));
      }
      this.lastVisit = Number(localStorage.getItem(`xedoc-last-active:${username}`)) || Date.now() / 1000;
    } catch {}
  }

  observe(session, workspaceId, projectName) {
    const previous = this.sessions.get(session.threadId);
    const saved = this.saved[session.threadId];
    const updatedAt = session.updatedAt ?? previous?.updatedAt ?? 0;
    const needsInput = Boolean(session.needsInput);
    const active = session.status === "active" || needsInput;
    const completed = !active && Boolean(
      previous?.completed
      || saved?.completed
      || previous?.active
      || (saved && updatedAt > saved.updatedAt)
      || (!saved && !previous && updatedAt > this.lastVisit)
    );
    const next = {
      ...session, workspaceId, projectName, updatedAt, active, completed, needsInput,
    };
    this.sessions.set(session.threadId, next);
    if (saved?.updatedAt !== updatedAt || saved?.completed !== completed) {
      this.saved[session.threadId] = { updatedAt, completed };
      this.persist();
    }
  }

  observeProjects(projects, workspaceId) {
    let observedActivity = false;
    for (const project of projects) {
      for (const session of project.sessionActivity || []) {
        this.observe(session, workspaceId, project.name);
        observedActivity = true;
      }
    }
    this.paint();
    if (!observedActivity) return;
    try {
      localStorage.setItem(`xedoc-last-active:${this.account}`, String(Date.now() / 1000));
      this.lastVisit = Date.now() / 1000;
    } catch {}
  }

  observeWorkspaces(workspaces) {
    const visible = new Set();
    for (const workspace of workspaces) {
      for (const session of workspace.sessions) {
        visible.add(session.threadId);
        this.observe(session, workspace.workspaceId, session.projectName);
      }
    }
    for (const threadId of this.sessions.keys()) {
      if (!visible.has(threadId)) this.sessions.delete(threadId);
    }
    this.paint();
  }

  attach(element, workspaceId, projectName, threadId = "") {
    const group = document.createElement("span");
    group.className = "session-state";
    group.dataset.workspace = workspaceId;
    group.dataset.project = projectName;
    group.dataset.thread = threadId;
    for (const [kind, title] of [
      ["running", "Turn active"],
      ["waiting", "Input or approval needed"],
      ["completed", "Turn finished"],
    ]) {
      const marker = document.createElement("span");
      marker.className = `state-${kind}`;
      marker.dataset.kind = kind;
      marker.setAttribute("role", "img");
      marker.setAttribute("aria-label", title);
      marker.title = title;
      marker.hidden = true;
      group.append(marker);
    }
    element.append(group);
  }

  markVisible() {
    this.paint();
  }

  markVisited(threadId) {
    const session = this.sessions.get(threadId);
    if (!session?.completed) return;
    session.completed = false;
    this.saved[threadId] = { updatedAt: session.updatedAt, completed: false };
    this.persist();
    this.paint();
  }

  paint() {
    for (const group of document.querySelectorAll(".session-state")) {
      const sessions = [...this.sessions.values()].filter((session) =>
        session.workspaceId === group.dataset.workspace
        && (!group.dataset.project || session.projectName === group.dataset.project)
        && (!group.dataset.thread || session.threadId === group.dataset.thread));
      const selectedTab = group.closest(".session-tab")
        ?.getAttribute("aria-selected") === "true";
      const states = {
        running: sessions.some((session) => session.active && !session.needsInput),
        waiting: sessions.some((session) => session.needsInput),
        completed: !selectedTab
          && sessions.some((session) => session.completed && !session.active),
      };
      group.hidden = !Object.values(states).some(Boolean);
      for (const marker of group.children) marker.hidden = !states[marker.dataset.kind];
    }
  }

  persist() {
    const entries = Object.entries(this.saved);
    if (entries.length > 1000) this.saved = Object.fromEntries(entries.slice(-1000));
    try {
      localStorage.setItem(`xedoc-activity:${this.account}`, JSON.stringify(this.saved));
    } catch {}
  }
};
