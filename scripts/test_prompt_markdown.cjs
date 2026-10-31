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
  async function visibleCaret() {
    const previous = await input.evaluate(node => {
      const properties = ["caret-color", "caret-animation"];
      const previous = properties.map(name =>
        [name, node.style.getPropertyValue(name), node.style.getPropertyPriority(name)]);
      node.style.setProperty("caret-color", "rgb(255, 0, 255)", "important");
      node.style.setProperty("caret-animation", "manual", "important");
      return previous;
    });
    try {
      let pixels = 0;
      for (let attempt = 0; attempt < 8; attempt++) {
        const screenshot = await input.screenshot({ caret: "initial" });
        pixels = await page.evaluate(async bytes => {
          const image = await createImageBitmap(new Blob([new Uint8Array(bytes)], { type: "image/png" }));
          const canvas = document.createElement("canvas");
          canvas.width = image.width;
          canvas.height = image.height;
          const context = canvas.getContext("2d");
          context.drawImage(image, 0, 0);
          image.close();
          const { data } = context.getImageData(0, 0, canvas.width, canvas.height);
          let pixels = 0;
          for (let index = 0; index < data.length; index += 4) {
            if (data[index] === 255 && data[index + 1] === 0 && data[index + 2] === 255) pixels++;
          }
          return pixels;
        }, [...screenshot]);
        if (pixels >= 10) return;
        await page.waitForTimeout(100);
      }
      assert.ok(pixels >= 10, "The focused prompt must paint a visible caret");
    } finally {
      await input.evaluate((node, properties) => {
        for (const [name, value, priority] of properties) node.style.setProperty(name, value, priority);
      }, previous);
    }
  }
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  for (const marker of ["-", "- "]) {
    await replace("Existing row");
    await page.keyboard.press("Shift+Enter");
    await page.keyboard.type(marker);
    assert.equal(await source(), `Existing row\n${marker}`);
    assert.equal(await input.locator(".editor-heading").count(), 0);
    assert.equal(await input.locator(".editor-list").count(), 1);
    assert.equal(await input.locator(".editor-list").evaluate(node =>
      getComputedStyle(node, "::before").content), '"•"');
    await page.keyboard.type(marker.endsWith(" ") ? "item" : " item");
    assert.equal(await source(), "Existing row\n- item");
    assert.equal(await input.locator(".editor-heading").count(), 0);
    assert.equal(await input.locator(".editor-list").count(), 1);
  }
  await replace("-");
  assert.equal(await input.locator(".editor-list").count(), 1);
  await page.keyboard.type(" parent");
  await page.keyboard.press("Shift+Enter");
  assert.equal(await input.locator(".editor-list").count(), 2);
  await visibleCaret();
  await page.keyboard.press("Tab");
  await visibleCaret();
  await page.keyboard.press("Shift+Tab");
  await visibleCaret();
  await page.keyboard.press("Tab");
  await page.keyboard.insertText("child");
  assert.equal(await source(), "- parent\n  - child");
  await replace("1. parent");
  await page.keyboard.press("Shift+Enter");
  await visibleCaret();
  await page.keyboard.press("Tab");
  await visibleCaret();
  await page.keyboard.insertText("child");
  assert.equal(await source(), "1. parent\n  1.1. child");
  await page.keyboard.press("Shift+Enter");
  await visibleCaret();
  await page.keyboard.insertText("sibling");
  assert.equal(await source(), "1. parent\n  1.1. child\n  1.2. sibling");
  await replace("- [ ] task");
  await page.keyboard.press("Shift+Enter");
  await visibleCaret();
  await page.keyboard.insertText("next");
  assert.equal(await source(), "- [ ] task\n- [ ] next");
  await page.keyboard.press("Shift+Enter");
  await visibleCaret();
  await page.keyboard.press("Shift+Enter");
  await visibleCaret();
  assert.equal(await source(), "- [ ] task\n- [ ] next\n");
  await replace("- parent");
  await page.keyboard.press("Shift+Enter");
  await visibleCaret();
  await page.keyboard.press("Backspace");
  assert.equal(await source(), "- parent");
  await visibleCaret();

  const text = [
    "- parent", "  - child", "1. parent", "  1.1. child", "",
    "# Heading", "---", "", "---", "---", "",
    "Existing row", "-", "", "Another row", "- ", "", "Continued row", "- item", "",
    "Setext heading", "--", "", "Top heading", "===", "", "First line", "Second line", "---", "",
    "| Name | Value |", "| --- | --- |", "| cell | `value` |", "",
    "    - code, not a list", "", "\\*literal\\*", "",
    "[link](https://example.com/Topic_(detail))", "",
    "```text", "[Image #1]", "```",
  ].join("\n");
  await replace(text);
  assert.deepEqual(await input.locator(".editor-heading").evaluateAll(nodes =>
    nodes.map(node => node.dataset.level)), ["1", "2", "1", "2", "2"]);
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
