"use strict";

// Port of DesktopNotifications.swift: input-needed and turn-finished alerts that open their session.
const { Notification } = require("electron");
const { validatedHostURL } = require("./urls");

function sessionRoute(info) {
  const host = validatedHostURL(info?.host);
  const text = (key, limit) => typeof info[key] === "string" && info[key].length > 0 && info[key].length <= limit;
  if (!host || !text("username", 64) || !text("workspaceId", 1024) || !text("projectName", 1024)
      || !text("threadId", 128)) return null;
  return {
    host: host.href, username: info.username, workspaceId: info.workspaceId,
    projectName: info.projectName, threadId: info.threadId,
  };
}

const delivered = new Map();
let openSession = () => {};

function onOpenSession(handler) {
  openSession = handler;
}

function receive(event, host, username) {
  const kind = event?.kind;
  if (!["inputNeeded", "turnFinished"].includes(kind) || !username || event.username !== username) return;
  const route = sessionRoute({ ...event, host });
  if (!route || !Notification.isSupported()) return;
  const identifier = `${host}:${username}:${route.threadId}:${kind}`;
  delivered.get(identifier)?.close();
  const hostname = new URL(host).hostname;
  const notification = new Notification({
    title: kind === "inputNeeded" ? "Xedoc needs your input" : "Xedoc turn finished",
    body: `${hostname} · ${route.projectName.slice(0, 120)}`,
  });
  // Keep a reference until the alert closes so its click handler stays alive.
  delivered.set(identifier, notification);
  notification.on("click", () => openSession(route));
  notification.on("close", () => {
    if (delivered.get(identifier) === notification) delivered.delete(identifier);
  });
  notification.on("failed", (_event, error) => console.error(`Xedoc notification delivery failed: ${error}`));
  notification.show();
}

module.exports = { sessionRoute, onOpenSession, receive };
