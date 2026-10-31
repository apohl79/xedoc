const assert = require("node:assert/strict");

// Pass a live Web UI page and a fixture that replaces only composer.timer in its frames.
module.exports = async function verifyWebUiTurnTimer(page, setElapsed) {
  await page.waitForFunction(() => document.querySelector("#input")?.isContentEditable);
  const results = [];
  for (const [seconds, expected] of [
    [null, ""],
    [0, ""],
    [59, "0m 59s"],
    [60, "1m 0s"],
    [3599, "59m 59s"],
    [3600, "1h 0m 0s"],
    [4342, "1h 12m 22s"],
    [6272, "1h 44m 32s"],
    [7199, "1h 59m 59s"],
    [7200, "2h 0m 0s"],
    [90061, "25h 1m 1s"],
    [null, ""],
    [1, "0m 1s"],
    [0, ""],
  ]) {
    await setElapsed(seconds);
    await page.waitForFunction(value =>
      document.querySelector("#turn-timer").textContent === value, expected);
    const observed = await page.locator("#turn-timer").textContent();
    assert.equal(observed, expected);
    results.push({ seconds, expected, observed });
  }
  return results;
};
