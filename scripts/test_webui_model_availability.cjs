const assert = require("node:assert/strict");

// Pass a live page with an alias-only provider and a genuinely restricted account model.
module.exports = async function verifyWebUiModelAvailability(page, {
  selectableModel, restrictedModel, marker,
}) {
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  await page.locator("#model").click();
  const search = page.getByRole("searchbox", { name: "Search models" });
  await search.fill(restrictedModel);
  const restricted = page.getByRole("menuitemradio");
  assert.equal(await restricted.count(), 1);
  assert.equal(await restricted.isDisabled(), true);
  assert.match(await restricted.textContent(), /unavailable on this backend/);
  await search.fill(selectableModel);
  const selectable = page.getByRole("menuitemradio");
  assert.equal(await selectable.count(), 1);
  assert.equal(await selectable.isDisabled(), false);
  assert.doesNotMatch(await selectable.textContent(), /unavailable/);
  await selectable.click();
  await page.waitForFunction(model =>
    document.querySelector("#model").textContent === model, selectableModel);
  await page.locator("#effort").click();
  await page.getByRole("menuitemradio", { name: "low", exact: true }).click();
  await page.waitForFunction(() => document.querySelector("#effort").textContent === "low");
  await page.locator("#input").fill(`Reply exactly ${marker}. Do not use tools.`);
  await page.locator("#input").press("Enter");
  await page.getByText(marker, { exact: true }).first().waitFor({ timeout: 120000 });
  await page.locator("#model").click();
  await search.fill(selectableModel);
  assert.equal(await page.getByRole("menuitemradio").isDisabled(), false);
  await page.keyboard.press("Escape");
  return { selectableModel, restrictedModel, marker };
};
