"use strict";

function displayHostName(hostname) {
  return hostname.replace(/^([^.]+)-[0-9]+(?=\.|$)/, "$1");
}

if (typeof module !== "undefined") module.exports = { displayHostName };
