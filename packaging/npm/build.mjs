#!/usr/bin/env node
// Assemble the codegraph npm packages into a staging directory. Never publishes.
//
//   node packaging/npm/build.mjs --archives dist --out dist/npm
//   node packaging/npm/build.mjs --binary win32-x64=target/release/codegraph.exe
//
// --archives DIR   directory with the release archives (codegraph-<ver>-<os>-<arch>.<zip|tar.gz>)
//                  and optionally SHA256SUMS, which is then verified. Missing platforms are
//                  skipped with a warning.
// --binary P=PATH  use a local binary for platform P (e.g. win32-x64) instead of an archive.
//                  Repeatable.
// --version X.Y.Z  package version (default: Cargo.toml's version).
// --out DIR        staging directory (default dist/npm), wiped first.
//
// Output: <out>/codegraph (the main package with the JS shim) and
// <out>/codegraph-<platform>-<arch> per platform, each ready for `npm pack` / `npm publish`
// (see docs/INSTALL.md). Uses only Node built-ins plus the system `tar` (and `unzip` where
// tar cannot read zip files).

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(here, "..", "..");

const PLATFORMS = [
  { key: "win32-x64", platform: "win32", arch: "x64", asset: "windows-amd64", ext: "zip", exe: "codegraph.exe" },
  { key: "linux-x64", platform: "linux", arch: "x64", asset: "linux-amd64", ext: "tar.gz", exe: "codegraph" },
  { key: "linux-arm64", platform: "linux", arch: "arm64", asset: "linux-arm64", ext: "tar.gz", exe: "codegraph" },
  { key: "darwin-x64", platform: "darwin", arch: "x64", asset: "macos-amd64", ext: "tar.gz", exe: "codegraph" },
  { key: "darwin-arm64", platform: "darwin", arch: "arm64", asset: "macos-arm64", ext: "tar.gz", exe: "codegraph" },
];

function die(msg) {
  process.stderr.write(`build.mjs: ${msg}\n`);
  process.exit(1);
}

function parseArgs(argv) {
  const opts = { archives: null, binaries: new Map(), version: null, out: path.join(repoRoot, "dist", "npm") };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      if (i + 1 >= argv.length) die(`${a} needs a value`);
      return argv[++i];
    };
    if (a === "--archives") opts.archives = path.resolve(next());
    else if (a === "--out") opts.out = path.resolve(next());
    else if (a === "--version") opts.version = next().replace(/^v/, "");
    else if (a === "--binary") {
      const v = next();
      const eq = v.indexOf("=");
      if (eq < 0) die(`--binary expects PLATFORM=PATH, got ${v}`);
      opts.binaries.set(v.slice(0, eq), path.resolve(v.slice(eq + 1)));
    } else if (a === "-h" || a === "--help") {
      process.stdout.write("usage: build.mjs [--archives DIR] [--binary P=PATH]... [--version X] [--out DIR]\n");
      process.exit(0);
    } else die(`unknown argument ${a}`);
  }
  if (!opts.archives && opts.binaries.size === 0) die("give --archives DIR and/or --binary P=PATH");
  for (const k of opts.binaries.keys()) {
    if (!PLATFORMS.some((p) => p.key === k)) die(`unknown platform ${k}`);
  }
  return opts;
}

function cargoVersion() {
  const toml = fs.readFileSync(path.join(repoRoot, "Cargo.toml"), "utf8");
  const m = toml.match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) die("no version in Cargo.toml");
  return m[1];
}

function sha256(file) {
  return createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function readSums(dir) {
  const f = path.join(dir, "SHA256SUMS");
  if (!fs.existsSync(f)) return null;
  const sums = new Map();
  for (const line of fs.readFileSync(f, "utf8").split(/\r?\n/)) {
    const m = line.trim().match(/^([0-9a-fA-F]{64})\s+\*?(.+)$/);
    if (m) sums.set(m[2], m[1].toLowerCase());
  }
  return sums;
}

function extract(archive, ext, dest) {
  fs.mkdirSync(dest, { recursive: true });
  if (ext === "tar.gz") {
    execFileSync("tar", ["-xzf", archive, "-C", dest], { stdio: "inherit" });
    return;
  }
  // bsdtar (Windows 10+, macOS) reads zip files; GNU tar does not.
  try {
    execFileSync("tar", ["-xf", archive, "-C", dest], { stdio: "pipe" });
  } catch {
    execFileSync("unzip", ["-o", "-q", archive, "-d", dest], { stdio: "inherit" });
  }
}

function fill(template, vars) {
  return template.replace(/\{\{(\w+)\}\}/g, (_, k) => {
    if (!(k in vars)) die(`template variable ${k} has no value`);
    return vars[k];
  });
}

function copyDocs(dir) {
  for (const f of ["README.md", "LICENSE"]) fs.copyFileSync(path.join(repoRoot, f), path.join(dir, f));
}

const opts = parseArgs(process.argv.slice(2));
const version = opts.version ?? cargoVersion();
const sums = opts.archives ? readSums(opts.archives) : null;
if (opts.archives && !sums) process.stderr.write("build.mjs: warning: no SHA256SUMS next to the archives; not verifying\n");

fs.rmSync(opts.out, { recursive: true, force: true });
fs.mkdirSync(opts.out, { recursive: true });
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "codegraph-npm-"));
const template = fs.readFileSync(path.join(here, "platform", "package.json.tmpl"), "utf8");
const built = [];

try {
  for (const p of PLATFORMS) {
    let bin = opts.binaries.get(p.key);
    if (!bin && opts.archives) {
      const name = `codegraph-${version}-${p.asset}.${p.ext}`;
      const archive = path.join(opts.archives, name);
      if (!fs.existsSync(archive)) {
        process.stderr.write(`build.mjs: warning: ${name} not found; skipping ${p.key}\n`);
        continue;
      }
      if (sums) {
        const want = sums.get(name);
        if (!want) die(`SHA256SUMS has no entry for ${name}`);
        const got = sha256(archive);
        if (got !== want) die(`checksum mismatch for ${name}: expected ${want}, got ${got}`);
      }
      const x = path.join(tmp, p.key);
      extract(archive, p.ext, x);
      bin = path.join(x, p.exe);
    }
    if (!bin) continue;
    if (!fs.existsSync(bin)) die(`${p.key}: binary ${bin} does not exist`);

    const dir = path.join(opts.out, `codegraph-${p.key}`);
    fs.mkdirSync(path.join(dir, "bin"), { recursive: true });
    fs.copyFileSync(bin, path.join(dir, "bin", p.exe));
    fs.chmodSync(path.join(dir, "bin", p.exe), 0o755);
    fs.writeFileSync(
      path.join(dir, "package.json"),
      fill(template, { platform: p.platform, arch: p.arch, version, exe: p.exe }),
    );
    fs.copyFileSync(path.join(here, "platform", "README.md"), path.join(dir, "README.md"));
    fs.copyFileSync(path.join(repoRoot, "LICENSE"), path.join(dir, "LICENSE"));
    built.push(p.key);
  }

  // Main package: same shim, version and optionalDependencies pinned to `version`.
  const mainSrc = path.join(here, "codegraph");
  const mainDir = path.join(opts.out, "codegraph");
  fs.mkdirSync(path.join(mainDir, "bin"), { recursive: true });
  fs.copyFileSync(path.join(mainSrc, "bin", "codegraph.js"), path.join(mainDir, "bin", "codegraph.js"));
  fs.chmodSync(path.join(mainDir, "bin", "codegraph.js"), 0o755);
  const pkg = JSON.parse(fs.readFileSync(path.join(mainSrc, "package.json"), "utf8"));
  pkg.version = version;
  pkg.optionalDependencies = Object.fromEntries(
    PLATFORMS.map((p) => [`@crowbett/codegraph-${p.key}`, version]),
  );
  fs.writeFileSync(path.join(mainDir, "package.json"), JSON.stringify(pkg, null, 2) + "\n");
  copyDocs(mainDir);
} finally {
  fs.rmSync(tmp, { recursive: true, force: true });
}

if (built.length === 0) die("no platform package was assembled");
const missing = PLATFORMS.filter((p) => !built.includes(p.key)).map((p) => p.key);
process.stdout.write(`staged codegraph ${version} in ${opts.out}\n  platforms: ${built.join(", ")}\n`);
if (missing.length) process.stdout.write(`  missing:   ${missing.join(", ")} (do not publish a partial set)\n`);
