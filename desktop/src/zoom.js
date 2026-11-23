"use strict";

const { app, screen, webContents } = require("electron");
const state = require("./state");

let level = 0;
const dialogs = new Map();

function resizeDialog(window, width, height) {
  const area = screen.getDisplayMatching(window.getBounds()).workArea;
  const factor = Math.pow(1.2, level);
  let maxWidth = area.width;
  let maxHeight = area.height - 32;
  const parent = window.getParentWindow();
  if (process.platform === "darwin" && parent && window.isModal()) {
    const anchor = parent.getContentBounds();
    const center = anchor.x + anchor.width / 2;
    maxWidth = Math.max(1, 2 * Math.min(center - area.x, area.x + area.width - center));
    maxHeight = Math.max(1, area.y + area.height - Math.max(area.y, anchor.y) - 32);
  }
  window.setContentSize(Math.min(maxWidth, Math.round(width * factor)),
    Math.min(maxHeight, Math.round(height * factor)));
  const bounds = window.getBounds();
  window.setPosition(
    Math.max(area.x, Math.min(bounds.x, area.x + area.width - bounds.width)),
    Math.max(area.y, Math.min(bounds.y, area.y + area.height - bounds.height)),
  );
}

function sizeDialog(window, width, height) {
  dialogs.set(window, { width, height });
  window.once("closed", () => dialogs.delete(window));
  window.once("show", () => resizeDialog(window, width, height));
  resizeDialog(window, width, height);
}

function changeLevel(next) {
  level = Math.max(-3, Math.min(5, next));
  state.set("uiZoomLevel", level);
  for (const contents of webContents.getAllWebContents()) {
    if (!contents.isDestroyed()) contents.setZoomLevel(level);
  }
  for (const [window, { width, height }] of dialogs) resizeDialog(window, width, height);
}

function installZoom() {
  const saved = state.get("uiZoomLevel");
  if (Number.isInteger(saved) && saved >= -3 && saved <= 5) level = saved;
  app.on("web-contents-created", (_event, contents) => {
    const apply = () => { if (!contents.isDestroyed()) contents.setZoomLevel(level); };
    apply();
    contents.on("did-finish-load", apply);
    contents.on("before-input-event", (event, input) => {
      const modifier = process.platform === "darwin" ? input.meta && !input.control : input.control && !input.meta;
      if (input.type !== "keyDown" || !modifier || input.alt) return;
      const action = ["+", "="].includes(input.key) ? 1 : input.key === "-" ? -1 : input.key === "0" ? 0 : null;
      if (action === null) return;
      event.preventDefault();
      if (!input.isAutoRepeat) changeLevel(action === 0 ? 0 : level + action);
    });
  });
}

function zoomMenu() {
  return [
    { label: "Zoom In", accelerator: "CommandOrControl+Plus", registerAccelerator: false, click: () => changeLevel(level + 1) },
    { label: "Zoom Out", accelerator: "CommandOrControl+-", registerAccelerator: false, click: () => changeLevel(level - 1) },
    { label: "Actual Size", accelerator: "CommandOrControl+0", registerAccelerator: false, click: () => changeLevel(0) },
  ];
}

module.exports = { installZoom, zoomMenu, sizeDialog };
