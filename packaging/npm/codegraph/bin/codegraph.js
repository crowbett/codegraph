#!/usr/bin/env node
// codegraph npm shim: runs the native binary from the platform package that npm
// installed through optionalDependencies (esbuild-style). Set CODEGRAPH_BINARY to a
// path to override the lookup (useful for testing a local build).
"use strict";

const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");

const SCOPE = "@crowbett";
const SUPPORTED = new Set(["win32-x64", "linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64"]);

function binaryPath() {
  const override = process.env.CODEGRAPH_BINARY;
  if (override) {
    if (!fs.existsSync(override)) {
      throw new Error(`CODEGRAPH_BINARY points at ${override}, which does not exist`);
    }
    return override;
  }
  const key = `${process.platform}-${process.arch}`;
  if (!SUPPORTED.has(key)) {
    throw new Error(
      `codegraph has no prebuilt binary for ${key}; supported: ${[...SUPPORTED].join(", ")}`,
    );
  }
  const pkg = `${SCOPE}/codegraph-${key}`;
  const exe = process.platform === "win32" ? "codegraph.exe" : "codegraph";
  try {
    return require.resolve(`${pkg}/bin/${exe}`);
  } catch {
    throw new Error(
      `the platform package ${pkg} is not installed. ` +
        "Reinstall without --no-optional / --omit=optional so npm can pick it up.",
    );
  }
}

let bin;
try {
  bin = binaryPath();
} catch (err) {
  process.stderr.write(`codegraph: ${err.message}\n`);
  process.exit(1);
}

// A platform package packed on Windows loses the executable bit; restore it once.
if (process.platform !== "win32") {
  try {
    fs.accessSync(bin, fs.constants.X_OK);
  } catch {
    try {
      fs.chmodSync(bin, 0o755);
    } catch {
      // Fall through: spawn reports EACCES with the path below.
    }
  }
}

const res = spawnSync(bin, process.argv.slice(2), { stdio: "inherit", windowsHide: true });
if (res.error) {
  process.stderr.write(`codegraph: failed to run ${bin}: ${res.error.message}\n`);
  process.exit(1);
}
if (res.signal) {
  process.kill(process.pid, res.signal);
}
process.exit(res.status ?? 1);
