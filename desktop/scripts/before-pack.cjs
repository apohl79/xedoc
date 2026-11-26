"use strict";

const { prepare } = require("./prepare-onepassword.cjs");
const buildMacOSHelper = require("./build-macos-helper.cjs");

let prepared;

module.exports = async (context) => {
  await (prepared ||= prepare());
  return buildMacOSHelper(context);
};
