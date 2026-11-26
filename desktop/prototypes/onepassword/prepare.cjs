"use strict";

const path = require("node:path");
const { prepare } = require("../../scripts/prepare-onepassword.cjs");

prepare({
  cache: path.join(__dirname, ".cache"),
  destination: path.join(__dirname, "extension"),
}).catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
