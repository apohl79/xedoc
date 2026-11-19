"use strict";

const api = window.xedoc;
const address = document.getElementById("address");

document.getElementById("form").addEventListener("submit", (event) => {
  event.preventDefault();
  api.invoke("open", address.value);
});
document.getElementById("cancel").addEventListener("click", () => api.invoke("cancel"));
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") api.invoke("cancel");
});
address.focus();
