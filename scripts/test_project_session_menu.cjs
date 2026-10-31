const assert = require("node:assert/strict");

// Pass an authenticated HostServer page with isolated alpha/beta projects and an archived fixture session.
module.exports = async function verifyProjectSessionMenu(page) {
  const trigger = page.locator("#session-menu-button");
  const menu = page.getByRole("menu", { name: "Project session actions" });
  const isOpen = () => page.waitForFunction(() => {
    const node = document.querySelector("#session-menu");
    return node.matches(":popover-open") && getComputedStyle(node).visibility === "visible";
  });
  const isClosed = () => page.waitForFunction(() =>
    !document.querySelector("#session-menu").matches(":popover-open")
      && document.querySelector("#session-menu-button").getAttribute("aria-expanded") === "false");
  const focused = id => page.waitForFunction(value => document.activeElement?.id === value, id);
  async function open() {
    await trigger.click();
    await isOpen();
    await focused("new-session");
    assert.equal(await trigger.getAttribute("aria-expanded"), "true");
    assert.equal(await menu.getByRole("menuitem").count(), 2);
    assert.equal(await page.locator("dialog:modal").count(), 0);
  }
  async function geometry() {
    const bounds = await menu.boundingBox();
    const anchor = await trigger.boundingBox();
    const viewport = page.viewportSize();
    assert(bounds.x >= 7 && bounds.y >= 7, JSON.stringify(bounds));
    assert(bounds.x + bounds.width <= viewport.width - 7, JSON.stringify(bounds));
    assert(bounds.y + bounds.height <= viewport.height - 7, JSON.stringify(bounds));
    assert(bounds.width < 250 && bounds.height < 130, JSON.stringify(bounds));
    assert(Math.abs(bounds.y - anchor.y - anchor.height - 4) < 1, JSON.stringify({bounds, anchor}));
    return {viewport, bounds, anchor};
  }
  const results = [];
  await open();
  results.push(await geometry());
  await page.locator("#project-title").click();
  await isClosed();
  await open();
  await page.keyboard.press("Escape");
  await isClosed();
  await focused("session-menu-button");
  await open();
  await trigger.click();
  await isClosed();
  await trigger.focus();
  await page.keyboard.press("Enter");
  await isOpen();
  await focused("new-session");
  await page.keyboard.press("ArrowDown");
  await focused("resume-session");
  await page.keyboard.press("ArrowDown");
  await focused("new-session");
  await page.keyboard.press("ArrowUp");
  await focused("resume-session");
  await page.keyboard.press("Home");
  await focused("new-session");
  await page.keyboard.press("End");
  await focused("resume-session");
  await page.keyboard.press("Tab");
  await isClosed();

  await open();
  await menu.getByRole("menuitem", {name: "New session", exact: true}).click();
  await isClosed();
  await page.locator('.session-tab[data-thread-id="new-1"][aria-selected="true"]').waitFor();
  await open();
  await page.frameLocator("#session-frames iframe:not([hidden])").locator("body").evaluate(node =>
    node.ownerDocument.defaultView.parent.postMessage({
      type: "xedoc-session-activity", title: "A longer fixture session title", turnActive: false,
    }, location.origin));
  await isClosed();
  await page.locator('.session-tab[data-thread-id="new-1"] .session-title')
    .filter({hasText: "A longer fixture session title"}).waitFor();
  await open();
  await page.frameLocator("#session-frames iframe:not([hidden])").locator("#session-control").click();
  await isClosed();
  assert.equal(await page.evaluate(() => document.activeElement?.tagName), "IFRAME");

  await trigger.focus();
  await page.keyboard.press("Space");
  await isOpen();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => document.querySelector("#resume-dialog").open);
  await isClosed();
  await page.locator("#resume-sessions").getByRole("button", {name: /Previous fixture session/}).click();
  await page.locator('.session-tab[data-thread-id="previous-session"][aria-selected="true"]').waitFor();
  assert.equal(await page.locator("#resume-dialog").evaluate(node => node.open), false);

  await open();
  await page.evaluate(() => document.querySelector('.project[data-name="beta"]').click());
  await isClosed();
  await page.waitForFunction(() => document.querySelector("#project-title").textContent === "beta");
  await open();
  await menu.getByRole("menuitem", {name: "New session", exact: true}).click();
  await page.locator('.session-tab[data-thread-id="new-2"][aria-selected="true"]').waitFor();

  for (const width of [390, 320, 1100]) {
    await open();
    await page.setViewportSize({width, height: 900});
    await isClosed();
    await open();
    results.push(await geometry());
    await page.keyboard.press("Escape");
    await isClosed();
  }
  await open();
  await page.locator("#logout").click();
  await isClosed();
  await page.locator("#login-view").waitFor();
  return results;
};
