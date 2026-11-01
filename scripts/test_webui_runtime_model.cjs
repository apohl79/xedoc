const assert = require("node:assert/strict");

// Pass a live routed thread; displayed runtime values must not replace its selected settings.
module.exports = async function verifyWebUiRuntimeModel(page, {
  model, effort, selectedModel, selectedEffort,
}) {
  await page.waitForFunction(expected =>
    document.querySelector("#model").textContent === expected.model
      && document.querySelector("#effort").textContent === expected.effort,
  { model, effort });
  await page.locator("#model").click();
  await page.getByRole("searchbox", { name: "Search models" }).fill(selectedModel);
  const selected = page.getByRole("menuitemradio");
  assert.equal(await selected.count(), 1);
  assert.equal(await selected.getAttribute("aria-checked"), "true");
  await page.keyboard.press("Escape");
  await page.locator("#effort").click();
  assert.equal(await page.getByRole("menuitemradio", {
    name: selectedEffort, exact: true,
  }).getAttribute("aria-checked"), "true");
  await page.keyboard.press("Escape");
  assert.equal(await page.locator("#model").textContent(), model);
  assert.equal(await page.locator("#effort").textContent(), effort);
  return { model, effort, selectedModel, selectedEffort };
};
