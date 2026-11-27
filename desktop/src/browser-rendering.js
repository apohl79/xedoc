"use strict";

async function waitForBackgroundFrame(view) {
  if (view.getBounds().x >= 0) return;
  const contents = view.webContents;
  contents.backgroundThrottling = false;
  let timeout;
  try {
    await Promise.race([
      contents.executeJavaScript("new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve(null))))"),
      new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error("The background page did not render in time.")), 3_000); }),
    ]);
  } finally {
    clearTimeout(timeout);
  }
}

module.exports = { waitForBackgroundFrame };
