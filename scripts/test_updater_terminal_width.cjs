const assert = require("node:assert/strict");

// Pass an authenticated host page with saved installer output in an isolated home.
module.exports = async function verifyUpdaterTerminalWidth(page) {
  const button = page.locator("#update-button");
  const dialog = page.locator("#update-dialog");
  await button.click();
  await page.locator("#update-terminal .xterm-screen").waitFor({ state: "visible" });
  const results = [];
  async function check() {
    await page.waitForFunction(() => {
      const dialog = document.querySelector("#update-dialog");
      const terminal = document.querySelector("#update-terminal");
      const screen = terminal.querySelector(".xterm-screen");
      const viewport = terminal.querySelector(".xterm-viewport");
      const style = getComputedStyle(terminal);
      const available = terminal.clientWidth - parseFloat(style.paddingLeft)
        - parseFloat(style.paddingRight) - (viewport.offsetWidth - viewport.clientWidth);
      const gap = available - screen.getBoundingClientRect().width;
      const bounds = dialog.getBoundingClientRect();
      return Math.abs(bounds.width - innerWidth * .6) < 1
        && Math.abs(bounds.left - (innerWidth - bounds.right)) < 1
        && dialog.scrollWidth === dialog.clientWidth
        && gap >= -1 && gap < 10 && terminal.scrollWidth === terminal.clientWidth;
    });
    const geometry = await dialog.evaluate(node => {
      const terminal = node.querySelector("#update-terminal");
      return { viewport: innerWidth, dialogWidth: node.getBoundingClientRect().width,
        left: node.getBoundingClientRect().left,
        right: innerWidth - node.getBoundingClientRect().right,
        terminalWidth: terminal.clientWidth,
        screenWidth: terminal.querySelector(".xterm-screen").getBoundingClientRect().width,
        overflow: terminal.scrollWidth - terminal.clientWidth };
    });
    assert.equal(geometry.overflow, 0);
    results.push(geometry);
  }
  for (const width of [2048, 1100, 390, 320, 1537, 2048]) {
    await page.setViewportSize({ width, height: 1100 });
    await check();
  }
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 1100 });
  await button.click();
  await check();
  await dialog.getByRole("button", { name: "Close", exact: true }).click();
  await page.setViewportSize({ width: 2048, height: 1100 });
  await button.click();
  await check();
  return results;
};
