const assert = require("node:assert/strict");

// Pass a live Web UI page with goals enabled, a local model mock, and its thread RPC fixture.
module.exports = async function verifyWebUiGoalStatus(page, rpc, threadId) {
  const input = page.locator("#input");
  const indicator = page.locator("#goal-status");
  const results = [];
  async function command(text) {
    await input.fill(text);
    await input.press("Enter");
  }
  async function expectLabel(pattern) {
    await page.waitForFunction(source =>
      new RegExp(source).test(document.querySelector("#goal-status").textContent), pattern.source);
    const label = await indicator.textContent();
    results.push(label);
    return label;
  }
  async function checkLayout(width) {
    await page.setViewportSize({ width, height: 850 });
    const layout = await indicator.evaluate(node => {
      const label = node.querySelector(".line > span");
      const status = document.querySelector("#status");
      const row = node.parentElement.getBoundingClientRect();
      const bounds = node.getBoundingClientRect();
      return {
        color: getComputedStyle(label).color,
        font: getComputedStyle(label).fontFamily,
        statusFont: getComputedStyle(status).fontFamily,
        size: getComputedStyle(label).fontSize,
        statusSize: getComputedStyle(status).fontSize,
        right: row.right - bounds.right,
        overlap: status.getBoundingClientRect().right > bounds.left,
        overflow: document.documentElement.scrollWidth > window.innerWidth,
      };
    });
    assert.equal(layout.color, "rgb(160, 107, 234)");
    assert.equal(layout.font, layout.statusFont);
    assert.equal(layout.size, layout.statusSize);
    assert.ok(layout.right >= 0 && layout.right <= 5, JSON.stringify(layout));
    assert.equal(layout.overlap, false);
    assert.equal(layout.overflow, false);
  }
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  assert.equal(await indicator.textContent(), "");
  await command("Local goal indicator acceptance. Reply OK without using tools.");
  await page.getByText("MARKDOWN_ACCEPTED", { exact: true }).first().waitFor();
  await command("/goal Verify the GUI goal status indicator");
  const initial = await expectLabel(/^Pursuing goal \(\d+s\)$/);
  await page.waitForFunction(previous =>
    document.querySelector("#goal-status").textContent !== previous, initial);
  assert.equal((await rpc("thread/goal/get", { threadId })).goal.status, "active");
  for (const width of [1100, 391, 320, 1100]) await checkLayout(width);
  await page.reload();
  await expectLabel(/^Pursuing goal \(/);
  await command("/goal pause");
  await expectLabel(/^Goal paused \(\/goal resume\)$/);
  await command("/goal resume");
  await expectLabel(/^Pursuing goal \(/);
  await command("/goal clear");
  await expectLabel(/^$/);
  await rpc("thread/goal/set", {
    threadId, objective: "Budgeted GUI indicator acceptance", status: "paused", tokenBudget: 20000,
  });
  await expectLabel(/^Goal paused \(\/goal resume\)$/);
  await command("/goal resume");
  await expectLabel(/^Pursuing goal \(.+ \/ .+\)$/);
  await rpc("thread/goal/set", { threadId, status: "complete" });
  await expectLabel(/^Goal achieved \(/);
  await command("/goal clear");
  await expectLabel(/^$/);
  assert.equal((await rpc("thread/goal/get", { threadId })).goal, null);
  return results;
};
