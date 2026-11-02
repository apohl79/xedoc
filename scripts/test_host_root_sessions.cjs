const assert = require("node:assert/strict");

// Pass an authenticated isolated HostServer page with the fixture sessions below.
module.exports = async function verifyHostRootSessions(page, fixture) {
  const project = {workspaceId: "fixture", projectName: "project"};
  const results = [];
  async function api(path, body) {
    return page.evaluate(async ({path, body}) => {
      const response = await fetch(`/api/${path}`, body === undefined ? {} : {
        method: "POST",
        headers: {"Content-Type": "application/json"},
        body: JSON.stringify(body),
      });
      return {status: response.status, body: await response.json()};
    }, {path, body});
  }
  function ids(sessions) {
    return sessions.map(session => session.threadId).sort();
  }
  await page.locator("#workspaces button").filter({hasText: "fixture"}).click();
  await page.locator('.project[data-name="project"]').click();
  await page.locator('.session-tab[data-thread-id="root-recent"]').waitFor();
  await page.frameLocator("#session-frames iframe:not([hidden])")
    .getByText("Isolated root session").waitFor();
  assert.deepEqual(await page.locator(".session-tab").evaluateAll(
    nodes => nodes.map(node => node.dataset.threadId).sort()
  ), ["root-old-active", "root-recent"]);
  results.push("project tabs contain recent and old-active roots only");

  await page.locator("#session-menu-button").click();
  await page.locator("#resume-session").click();
  await page.locator("#resume-sessions button").nth(2).waitFor();
  const titles = await page.locator("#resume-sessions button").allTextContents();
  assert.equal(titles.length, 3);
  assert(titles.every(title => title.includes("Root")));
  assert(titles.some(title => title.includes("Root archived")));
  await page.screenshot({path: `${fixture.base}/root-session-overlay.png`});
  await page.locator("#resume-sessions button").filter({hasText: "Root archived"}).click();
  await page.locator('.session-tab[data-thread-id="root-archived"][aria-selected="true"]').waitFor();
  results.push("resume overlay excludes children and resumes archived root");

  const history = await api("project/sessions?workspaceId=fixture&projectName=project&includeHistory=1");
  assert.equal(history.status, 200);
  assert.deepEqual(ids(history.body.sessions), ["root-archived", "root-old-active", "root-recent"]);
  const activity = await api("activity");
  assert.equal(activity.status, 200);
  const inventory = activity.body.workspaces;
  assert.deepEqual(ids(inventory.find(entry => entry.workspaceId === "fixture").sessions),
    ["root-archived", "root-old-active", "root-recent"]);
  assert.deepEqual(ids(inventory.find(entry => entry.workspaceId === "@other").sessions),
    ["root-outside"]);
  const outside = await api(`project/sessions?workspaceId=%40other&projectName=${encodeURIComponent(fixture.outside)}`);
  assert.equal(outside.status, 200);
  assert.deepEqual(ids(outside.body.sessions), ["root-outside"]);
  results.push("workspace, history, loaded fallback, and Other folders inventories exclude children");

  for (const threadId of ["child-recent", "child-old-active", "child-parent-only", "child-loaded-only", "child-archived"]) {
    for (const action of ["open", "gui"]) {
      const response = await api(`session/${action}`, {...project, threadId});
      assert.equal(response.status, 404, JSON.stringify({threadId, action, response}));
    }
  }
  results.push("direct top-level open and GUI requests reject all child forms");
  await page.reload();
  await page.locator(".session-tab").first().waitFor();
  const restored = await page.locator(".session-tab").evaluateAll(
    nodes => nodes.map(node => node.dataset.threadId)
  );
  assert(restored.length > 0 && restored.every(id => id.startsWith("root-")));
  results.push("browser reload restores root tabs only");
  return results;
};
