const assert = require("node:assert/strict");

// Pass a real child GUI while its mock turn is held, then release and await parking.
module.exports = async function verifyParkedRouterTables(page, fixture) {
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.goto(fixture.url);
  await page.locator("#input, #live").first().waitFor();
  if (await page.locator("#live").isVisible()) await page.locator("#live").press("Escape");
  await page.locator("#model").click();
  await page.locator('#menu [role="menuitemradio"]').first().waitFor();
  await page.keyboard.press("Escape");
  await page.locator("#routerTable").click();
  const choices = page.locator('#menu [role="menuitemradio"]');
  await choices.nth(1).waitFor();
  const options = await choices.evaluateAll(nodes => nodes.map(node => ({
    title: node.textContent.trim(),
    checked: node.getAttribute("aria-checked") === "true",
    disabled: node.disabled,
  })));
  const current = options.find(option => option.checked);
  const alternate = options.find(option => !option.checked && !option.disabled);
  assert(current && alternate, JSON.stringify(options));
  await page.keyboard.press("Escape");
  await fixture.releaseAndAwaitParking();
  const before = await page.locator("#history").innerText();
  for (const option of [alternate, current]) {
    await page.locator("#routerTable").click();
    const choice = page.locator('#menu [role="menuitemradio"]').filter({
      hasText: option.title,
    });
    await choice.click();
    await page.locator("#routerTable").filter({hasText: option.title}).waitFor();
    await fixture.verifySelectedTable(option.title);
  }
  await page.locator("#model").click();
  await page.locator('#menu [role="menuitemradio"]').first().waitFor();
  await page.keyboard.press("Escape");
  await page.waitForTimeout(1000);
  const after = await page.locator("#history").innerText();
  assert(!after.includes("Could not load routing tables"), after);
  assert(!after.includes("Failed to read model-router mode"), after);
  assert(!after.includes("no thread with id"), after);
  assert(after.includes(before), "Saved child history remains visible");
  assert.deepEqual(errors, []);
  return {current: current.title, alternate: alternate.title, errors};
};
