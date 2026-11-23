"use strict";

// Port of PairDeviceView.swift; requests go through the main process (local access or the host page).
const api = window.xedoc;
const $ = (id) => document.getElementById(id);
$("share").hidden = api.platform !== "darwin";
if (api.platform === "darwin") {
  $("local-help").textContent = "For this Mac’s host, connect using This Mac in Hosts. For another host, sign in as an administrator and retry.";
}
const model = {
  origin: "", enabled: null, canInvite: false, bridgeStatus: "", suggestedPort: null, invitation: "",
  invitationID: "", preview: null, expiresAt: 0, status: "pending", failure: null, working: false,
};

async function request(path, body = null) {
  const response = await api.invoke("request", path, body);
  if (response.error) throw new Error(response.error);
  return response.result;
}

function suggestedPort(text) {
  try {
    const url = new URL(text);
    const port = Number(url.port);
    return url.protocol === "https:" && url.hostname === "sendnix.de" && port >= 50001 && port <= 50010
      && `https://sendnix.de:${port}` === text ? port : null;
  } catch {
    return null;
  }
}

async function update(create) {
  model.working = true;
  model.failure = null;
  render();
  try {
    const result = await request(create ? "device/invite" : "device/pairing", create ? {} : null);
    if (create) {
      const { invitation, invitationId, expiresAt } = result;
      if (typeof invitation !== "string" || typeof invitationId !== "string" || typeof expiresAt !== "number") {
        throw new Error("Unexpected invitation from the host.");
      }
      const preview = await api.invoke("preview", invitation);
      if (preview.error) throw new Error(preview.error);
      Object.assign(model, {
        invitation, preview, expiresAt, status: "pending", invitationID: invitationId, enabled: true,
      });
      poll(invitationId);
    } else {
      if (typeof result.enabled !== "boolean") {
        throw new Error("Update the host to a version that supports device pairing.");
      }
      const setup = await request("device/bridge-setup");
      if (!["ready", "mismatch", "unreachable", "unconfigured"].includes(setup.status)) {
        throw new Error("Update the host to check bridge port availability.");
      }
      model.bridgeStatus = setup.status;
      model.suggestedPort = typeof setup.suggestedOrigin === "string" ? suggestedPort(setup.suggestedOrigin) : null;
      model.enabled = result.enabled;
      model.canInvite = (typeof result.canInvite === "boolean" ? result.canInvite : result.enabled)
        && setup.status === "ready";
    }
  } catch (error) {
    if (create) Object.assign(model, { canInvite: false, invitation: "", invitationID: "", preview: null });
    model.failure = error.message;
  }
  model.working = false;
  render();
}

async function poll(invitationID) {
  while (model.invitationID === invitationID && model.status === "pending") {
    await new Promise((resolve) => setTimeout(resolve, 2_000));
    if (model.invitationID !== invitationID) return;
    try {
      const result = await request("device/pairing-status", { invitationId: invitationID });
      if (model.invitationID !== invitationID) return;
      if (!["pending", "completed", "expired", "replaced"].includes(result.status)) {
        throw new Error("Unexpected pairing status from the host.");
      }
      model.status = result.status;
      model.failure = null;
    } catch (error) {
      if (model.invitationID !== invitationID) return;
      model.failure = error.message;
      if (Date.now() >= model.expiresAt * 1000) model.status = "expired";
    }
    render();
  }
}

function render() {
  const { status, preview, working } = model;
  const completed = status === "completed";
  $("step").textContent = !model.invitation ? `Step 1 of 3 · On this ${api.platform === "darwin" ? "Mac" : "computer"}`
    : completed ? "Step 3 of 3 · On the other device" : "Step 2 of 3 · On the other device";
  $("origin").textContent = preview?.origin ?? model.origin;
  $("completed").hidden = !completed;
  $("invitation").hidden = completed || !preview;
  $("ready").hidden = completed || Boolean(preview) || !model.canInvite;
  $("setup").hidden = completed || Boolean(preview) || model.canInvite || model.enabled === null;
  $("checking").hidden = completed || Boolean(preview) || model.canInvite || model.enabled !== null
    || Boolean(model.failure);
  if (preview) {
    $("fingerprint").textContent = preview.fingerprint;
    $("copy").disabled = status !== "pending";
    $("share").disabled = status !== "pending";
    $("pending").hidden = status !== "pending";
    const time = new Date(model.expiresAt * 1000).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    $("expires").textContent = `Single use · Expires ${time}. Creating another invitation replaces this one.`;
    $("ended").hidden = status === "pending";
    $("ended").textContent = status === "expired"
      ? "This invitation expired. Create another invitation, then paste and verify it again on the other device."
      : "Another invitation replaced this one. Use the newest invitation, or create another here.";
  }
  $("mismatch").hidden = model.bridgeStatus !== "mismatch";
  $("unreachable").hidden = model.bridgeStatus !== "unreachable";
  $("checking-ports").hidden = !working;
  $("suggested").hidden = working || !model.suggestedPort;
  $("no-port").hidden = working || Boolean(model.suggestedPort);
  if (model.suggestedPort) {
    $("suggested-port").textContent = `Port ${model.suggestedPort} appears unused on sendnix.de.`;
    $("suggested-intro").textContent = model.bridgeStatus === "unreachable"
      ? "Only if you intend to change this host’s port:" : "Run on the host you want to reach:";
    $("suggested-command").textContent =
      `xedoc-remote-agentd device-pairing setup --bridge-origin https://sendnix.de:${model.suggestedPort}`;
  }
  $("failure").hidden = !model.failure;
  $("failure").firstElementChild.textContent = model.failure ?? "";
  $("close").textContent = completed ? "Done" : "Close";
  $("close").disabled = working;
  $("working").hidden = !working;
  $("action").hidden = completed;
  $("action").disabled = working;
  $("action").textContent = model.canInvite ? (model.invitation ? "New invitation" : "Create invitation") : "Check again";
}

$("copy").addEventListener("click", () => api.invoke("copy", model.invitation));
$("share").addEventListener("click", () => api.invoke("share", model.invitation));
$("close").addEventListener("click", () => api.invoke("close"));
$("action").addEventListener("click", () => update(model.canInvite));
window.addEventListener("beforeunload", (event) => {
  if (model.working) event.returnValue = false;
});

api.invoke("origin").then((origin) => {
  model.origin = origin;
  render();
  update(false);
});
