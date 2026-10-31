const assert = require("node:assert/strict");

// Pass a Playwright page for an isolated Web UI session backed by a local model mock.
// Its browser context needs clipboard-read and clipboard-write permissions.
module.exports = async function verifyPromptMarkdown(page) {
  const input = page.locator("#input");
  const modifier = process.platform === "darwin" ? "Meta" : "Control";
  const source = () => input.evaluate(node =>
    [...node.querySelectorAll(".editor-line")].map(line => line.textContent).join("\n"));
  const waitForSource = expected => page.waitForFunction(value =>
    [...document.querySelectorAll("#input .editor-line")]
      .map(line => line.textContent).join("\n") === value, expected);
  const waitForClipboard = expected => page.waitForFunction(async value =>
    await navigator.clipboard.readText() === value, expected);
  async function replace(text) {
    await input.click();
    await page.keyboard.press(`${modifier}+A`);
    await page.keyboard.insertText(text);
    assert.equal(await source(), text);
  }
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  await replace("-");
  assert.equal(await input.locator(".editor-list").count(), 1);
  await page.keyboard.type(" parent");
  await page.keyboard.press("Shift+Enter");
  assert.equal(await input.locator(".editor-list").count(), 2);
  await page.keyboard.press("Tab");
  await page.keyboard.insertText("child");
  assert.equal(await source(), "- parent\n  - child");
  await replace("1. parent");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.press("Tab");
  await page.keyboard.insertText("child");
  assert.equal(await source(), "1. parent\n  1.1. child");
  await page.keyboard.press("Shift+Enter");
  await page.keyboard.insertText("sibling");
  assert.equal(await source(), "1. parent\n  1.1. child\n  1.2. sibling");

  const text = [
    "- parent", "  - child", "1. parent", "  1.1. child", "",
    "# Heading", "---", "", "---", "---", "",
    "| Name | Value |", "| --- | --- |", "| cell | `value` |", "",
    "    - code, not a list", "", "\\*literal\\*", "",
    "[link](https://example.com/Topic_(detail))", "",
    "```text", "[Image #1]", "```",
  ].join("\n");
  await replace(text);
  assert.equal(await input.locator('[role="table"]').count(), 1);
  assert.equal(await input.locator('[role="separator"]').count(), 3);
  assert.equal(await input.locator(".editor-code.editor-list").count(), 0);
  assert.equal(await input.locator('a[href="https://example.com/Topic_(detail)"]').count(), 1);
  const rendered = await input.innerHTML();
  await page.keyboard.press("Enter");
  await page.waitForFunction(expected =>
    [...document.querySelectorAll(".history-user[data-prompt]")]
      .some(node => node.dataset.prompt === expected), text);
  const history = page.locator(".history-user[data-prompt]").last();
  assert.equal(await history.locator(".prompt-markdown").innerHTML(), rendered);
  await page.reload();
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  await page.waitForFunction(expected =>
    [...document.querySelectorAll(".history-user[data-prompt]")]
      .some(node => node.dataset.prompt === expected), text);
  await input.click();
  await page.keyboard.press("ArrowUp");
  await page.waitForFunction(expected =>
    [...document.querySelectorAll("#input .editor-line")]
      .map(line => line.textContent).join("\n") === expected, text);
  await page.keyboard.press(`${modifier}+A`);
  await page.keyboard.press(`${modifier}+C`);
  await waitForClipboard(text);
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), text);
  await page.evaluate(() => navigator.clipboard.writeText("clipboard sentinel"));
  await page.keyboard.press(`${modifier}+X`);
  await waitForSource("");
  await waitForClipboard(text);
  assert.equal(await source(), "");
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), text);
  await page.keyboard.press(`${modifier}+V`);
  await waitForSource(text);
  assert.equal(await source(), text);
};
