"use strict";

const api = window.xedoc;
const $ = (id) => document.getElementById(id);

function render(value) {
  $("message").textContent = value?.error ?? "";
  $("certificate").hidden = !value?.certificateError;
}

api.on("state", render);
api.invoke("state").then(render);
for (const action of ["hosts", "retry", "dismiss"]) $(action).addEventListener("click", () => api.invoke(action));
$("certificate").addEventListener("click", () => api.invoke("certificateSetup"));
