"use strict";

// Port of HostSelector.swift and HostPairingView.swift. Host data is only ever set as textContent.
const api = window.xedoc;
const $ = (id) => document.getElementById(id);
const icons = {
  scanning: null,
  empty: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8.5 8.5a5 5 0 0 0 0 7M15.5 8.5a5 5 0 0 1 0 7M5.6 5.6a9 9 0 0 0 0 12.8M18.4 5.6a9 9 0 0 1 0 12.8"/><circle cx="12" cy="12" r="1"/></svg>',
  done: '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="m8 12 3 3 5-6"/></svg>',
};
let localHost = null;
let discovery = { hosts: [], scanning: true, status: "Searching local network…" };

function hostRow(host) {
  const row = $("host-row").content.firstElementChild.cloneNode(true);
  row.querySelector(".name").textContent = displayHostName(host.hostname);
  row.querySelector(".url").textContent = host.url;
  row.querySelector(".route").textContent = host.route === "bridge" ? "Bridge" : "Direct";
  row.addEventListener("click", () => api.invoke("select", host.url));
  return row;
}

function statusRow() {
  const row = $("status-row").content.firstElementChild.cloneNode(true);
  const icon = row.querySelector(".icon");
  if (discovery.scanning) {
    const spinner = document.createElement("span");
    spinner.className = "spinner";
    icon.append(spinner);
  } else {
    icon.innerHTML = discovery.hosts.length ? icons.done : icons.empty;
  }
  row.querySelector(".title").textContent = discovery.scanning ? "Searching for hosts"
    : discovery.hosts.length ? "Discovery complete" : "No hosts found";
  row.querySelector(".detail").textContent = discovery.scanning ? "Xedoc is scanning your local network." : discovery.status;
  return row;
}

function render() {
  const local = $("local-section");
  local.hidden = !localHost;
  local.replaceChildren(local.firstElementChild, ...(localHost ? [hostRow(localHost)] : []));
  const available = $("available");
  available.replaceChildren(available.firstElementChild, ...discovery.hosts.map(hostRow), statusRow());
}

api.on("discovery", (snapshot) => {
  discovery = snapshot;
  render();
});

$("refresh").addEventListener("click", async () => {
  localHost = await api.invoke("refresh");
  render();
});

$("address").addEventListener("input", () => {
  $("connect-button").disabled = !$("address").value.trim();
  $("invalid-address").hidden = true;
});

$("connect").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!$("address").value.trim()) return;
  $("invalid-address").hidden = await api.invoke("connect", $("address").value);
});

// Pair a host: 0 instructions, 1 paste, 2 compare, 3 paired.
const wizard = { step: 0, preview: null, url: null, working: false, failure: null };

function renderWizard() {
  const { step } = wizard;
  for (const panel of document.querySelectorAll("[data-step]")) panel.hidden = Number(panel.dataset.step) !== step;
  $("pairing-step").textContent = step === 3 ? "Pairing complete" : `Step ${step + 1} of 3`;
  $("invalid-invitation").hidden = step !== 1 || !$("invitation").value || Boolean(wizard.preview);
  $("preview-origin").textContent = wizard.preview?.origin ?? "";
  $("preview-fingerprint").textContent = wizard.preview?.fingerprint ?? "";
  $("paired-origin").textContent = wizard.url ? new URL(wizard.url).origin : "";
  $("pairing-failure").hidden = !wizard.failure;
  $("pairing-failure").firstElementChild.textContent = wizard.failure ?? "";
  $("pairing-cancel").hidden = step === 3;
  $("pairing-back").hidden = step === 0 || step === 3;
  $("pairing-cancel").disabled = wizard.working;
  $("pairing-back").disabled = wizard.working;
  $("pairing-working").hidden = !wizard.working;
  const next = $("pairing-next");
  next.textContent = ["I have an invitation", "Verify host", "Pair this device", "Connect to host"][step];
  next.disabled = (step === 1 && !wizard.preview) || (step === 2 && (!$("verified").checked || wizard.working));
}

$("pair-host").addEventListener("click", () => {
  Object.assign(wizard, { step: 0, preview: null, url: null, working: false, failure: null });
  $("invitation").value = "";
  $("verified").checked = false;
  renderWizard();
  $("pairing").showModal();
});

$("pairing").addEventListener("cancel", (event) => {
  if (wizard.working || wizard.step === 3) event.preventDefault();
});

$("pairing").addEventListener("close", () => {
  $("invitation").value = "";
  wizard.preview = null;
  api.invoke("pairingClosed");
});

$("pairing-cancel").addEventListener("click", () => $("pairing").close());

$("pairing-back").addEventListener("click", () => {
  wizard.step -= 1;
  wizard.failure = null;
  $("verified").checked = false;
  renderWizard();
});

$("invitation").addEventListener("input", async () => {
  const text = $("invitation").value;
  wizard.failure = null;
  $("verified").checked = false;
  const preview = text ? await api.invoke("previewInvitation", text) : null;
  if ($("invitation").value !== text) return;
  wizard.preview = preview && !preview.error ? preview : null;
  renderWizard();
});

$("verified").addEventListener("change", renderWizard);

$("pairing-next").addEventListener("click", async () => {
  if (wizard.step < 2) {
    wizard.step += 1;
  } else if (wizard.step === 2) {
    wizard.working = true;
    wizard.failure = null;
    renderWizard();
    const result = await api.invoke("pair", $("invitation").value);
    wizard.working = false;
    if (result.error) {
      wizard.failure = result.error;
    } else {
      wizard.url = result.url;
      wizard.step = 3;
      $("invitation").value = "";
    }
  } else {
    const url = wizard.url;
    $("pairing").close();
    api.invoke("select", url);
    return;
  }
  renderWizard();
});

api.invoke("state").then((value) => {
  localHost = value.localHost;
  discovery = value.discovery;
  render();
});
api.invoke("refresh").then((value) => {
  localHost = value;
  render();
});
let localHostPolls = 0;
const localHostPoll = setInterval(async () => {
  if (localHost || ++localHostPolls > 30) {
    clearInterval(localHostPoll);
    return;
  }
  localHost = await api.invoke("localHost");
  if (localHost) render();
}, 1000);
