#!/usr/bin/env node
// Runs the native otto binary for this system. The release workflow puts one
// binary per platform next to this file, in `<os>-<arch>/otto`.
"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const supported = ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64"];
const system = `${process.platform}-${process.arch}`;

if (!supported.includes(system)) {
  console.error(`otto does not support ${system}. Supported: ${supported.join(", ")}.`);
  process.exit(1);
}

const binary = path.join(__dirname, system, "otto");
if (!fs.existsSync(binary)) {
  console.error(`otto: the ${system} binary is missing from this install. Reinstall the package.`);
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (result.error) {
  console.error(`otto: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
