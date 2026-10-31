const assert = require("node:assert/strict");

// Pass a Playwright page connected to an isolated session with the reference router enabled.
module.exports = async function verifyWebUiFrameFit(page) {
  const input = page.locator("#input");
  const live = page.locator("#live");
  async function open() {
    await input.fill("/model-router");
    await input.press("Enter");
    await live.waitFor({ state: "visible" });
    await page.waitForFunction(() =>
      document.querySelector("#live-frame").textContent.includes("Model Router Settings"));
  }
  async function check() {
    await page.waitForFunction(() => {
      const live = document.querySelector("#live");
      const frame = document.querySelector("#live-frame");
      const probe = document.createElement("span");
      probe.style.cssText = `position:absolute;visibility:hidden;font:${getComputedStyle(live).font};width:100ch`;
      document.body.append(probe);
      const cell = probe.getBoundingClientRect().width / 100;
      probe.remove();
      const bounds = live.getBoundingClientRect();
      const rows = [...frame.querySelectorAll(".line")].map(line => {
        const range = document.createRange();
        range.selectNodeContents(line);
        return range.getBoundingClientRect();
      });
      const row = rows.reduce((widest, row) => row.width > widest.width ? row : widest,
        { width: 0 });
      const columns = Math.max(20, Math.min(400, Math.floor(live.clientWidth / cell)));
      return row.width > 0 && Math.abs(row.width - columns * cell) < 1
        && Math.abs((row.left - bounds.left) - (bounds.right - row.right)) < 1;
    });
    const geometry = await live.evaluate(node => {
      const frame = node.querySelector("#live-frame");
      const outer = node.getBoundingClientRect();
      const inner = frame.getBoundingClientRect();
      return { width: outer.width, frameWidth: inner.width,
        left: inner.left - outer.left, right: outer.right - inner.right,
        overflow: node.scrollWidth - node.clientWidth };
    });
    assert.ok(Math.abs(geometry.left - geometry.right) < 1, JSON.stringify(geometry));
    assert.equal(geometry.overflow, 0);
    return geometry;
  }
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  await open();
  const results = [];
  for (const width of [2048, 3500, 1100, 391, 320, 1537, 2048]) {
    await page.setViewportSize({ width, height: 1000 });
    results.push({ viewport: width, ...await check() });
  }
  await live.evaluate(node => node.style.fontSize = "15px");
  await page.evaluate(() => window.dispatchEvent(new Event("resize")));
  results.push({ fontSize: 15, ...await check() });
  await live.evaluate(node => node.style.fontSize = "");
  await page.evaluate(() => window.dispatchEvent(new Event("resize")));
  await check();
  await live.press("Escape");
  await input.waitFor({ state: "visible" });
  await open();
  await check();
  await live.press("Escape");
  await input.waitFor({ state: "visible" });
  return results;
};
