#!/usr/bin/env node
// Cache keys and pre-save pruning for the cargo caches in engine-ci, flake-hunter, publish and
// release-cache. The Actions cache holds 10 GiB per repository; keys that changed on every run
// (run id) wrote ~10 GB per main push and evicted everything within hours.
//
//   node .github/scripts/cargo-cache.mjs key <Cargo.lock>...   >> "$GITHUB_OUTPUT"
//     rust=<rustc version>   (for restore-keys: same compiler, other dependencies)
//     deps=<the hash alone>  (for the registry cache, which no compiler change affects)
//     key=<rustc version>-<hash of the locks' registry/git packages>
//     A release bump changes only our own crates' versions in Cargo.lock, so it keeps the key; a
//     dependency change or a new rustc makes a new one. Run after the Rust toolchain step.
//
//   node .github/scripts/cargo-cache.mjs prune <Cargo.lock> <target dir>
//     Removes our own crates' build output (every path package in the lock) before a save. Cargo
//     rebuilds path packages on every CI run anyway (it fingerprints them by mtime, and checkout
//     makes every file new), so saving them only made the cache bigger: test binaries were most of
//     the 3.3 GB engine test cache.
//
//   node .github/scripts/cargo-cache.mjs tauri-cli <package-lock.json>
//     key=<@tauri-apps/cli version>: what the generated Android project (and so Gradle's
//     dependency set) follows.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync, readdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// Cargo.lock as [[package]] blocks: { name, text, path } where `path` means no `source =` line
// (a workspace member or a path dependency such as a vendored crate).
export function lockPackages(lockText) {
  const blocks = lockText.replace(/\r\n/g, "\n").split(/\n(?=\[\[package\]\]\n)/);
  return blocks
    .filter((b) => b.startsWith("[[package]]"))
    .map((text) => ({
      name: (text.match(/^name = "([^"]+)"/m) || [])[1],
      text: text.trim(),
      path: !/^source = /m.test(text),
    }))
    .filter((p) => p.name);
}

export function pathPackages(lockText) {
  return [...new Set(lockPackages(lockText).filter((p) => p.path).map((p) => p.name))].sort();
}

// The registry and git packages only, in a stable order.
export function depsHash(lockTexts) {
  const h = createHash("sha256");
  for (const text of lockTexts) {
    for (const p of lockPackages(text).filter((p) => !p.path)) h.update(p.text + "\n");
    h.update("\0");
  }
  return h.digest("hex").slice(0, 16);
}

// "rustc 1.100.0 (58cf8f9c3 2026-10-13)" -> "rust1.100.0-58cf8f9c3"
export function rustcSlug(versionLine) {
  const m = versionLine.match(/^rustc (\S+)(?: \((\w+)[ )])?/);
  if (!m) throw new Error(`unexpected rustc -V output: ${versionLine}`);
  return `rust${m[1]}${m[2] ? `-${m[2]}` : ""}`.replace(/[^A-Za-z0-9._-]/g, "_");
}

// What to delete under one target dir: our crates' output in both cargo layouts.
//   Rust >= 1.100: <profile>/build/<pkg>/<hash>/...
//   older:         <profile>/{deps,.fingerprint,build}/<pkg or crate>-<16 hex>[...]
// plus uplifted files at the profile root, examples/, incremental/ and target/tmp (test scratch).
export function pruneList(targetDir, names, fs = { existsSync, readdirSync, statSync }) {
  const out = [];
  const crate = names.map((n) => n.replace(/-/g, "_"));
  const hashed = new RegExp(
    `^(?:lib)?(?:${[...names, ...crate].map(escape).join("|")})-[0-9a-f]{16}(?:\\..*)?$`,
  );
  const uplifted = new RegExp(
    `^(?:lib)?(?:${[...names, ...crate].map(escape).join("|")})(?:\\.[A-Za-z0-9]+)?$`,
  );
  const ls = (d) => (fs.existsSync(d) ? fs.readdirSync(d) : []);
  const isDir = (p) => fs.statSync(p).isDirectory();
  const profiles = [];
  for (const top of ls(targetDir)) {
    const p = join(targetDir, top);
    if (top === "tmp") {
      out.push(p);
      continue;
    }
    if (!isDir(p)) continue;
    if (isProfile(p, ls)) profiles.push(p);
    else for (const sub of ls(p)) if (isDir(join(p, sub)) && isProfile(join(p, sub), ls)) profiles.push(join(p, sub));
  }
  for (const prof of profiles) {
    for (const d of ["examples", "incremental"]) if (fs.existsSync(join(prof, d))) out.push(join(prof, d));
    for (const e of ls(join(prof, "build"))) {
      if (names.includes(e) || hashed.test(e)) out.push(join(prof, "build", e));
    }
    for (const d of ["deps", ".fingerprint"]) {
      for (const e of ls(join(prof, d))) if (hashed.test(e)) out.push(join(prof, d, e));
    }
    for (const e of ls(prof)) {
      if (uplifted.test(e) && !isDir(join(prof, e))) out.push(join(prof, e));
    }
  }
  return out;
}

function isProfile(dir, ls) {
  const entries = ls(dir);
  return entries.includes("build") || entries.includes("deps") || entries.includes(".fingerprint");
}

function escape(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function duBytes(p) {
  try {
    const s = statSync(p);
    if (!s.isDirectory()) return s.size;
    return readdirSync(p).reduce((n, e) => n + duBytes(join(p, e)), 0);
  } catch {
    return 0;
  }
}

function main(argv) {
  const [cmd, ...args] = argv;
  if (cmd === "key" && args.length) {
    const rustc = execFileSync("rustc", ["-V"], { encoding: "utf8" }).trim();
    const locks = args.map((f) => readFileSync(f, "utf8"));
    console.log(`rust=${rustcSlug(rustc)}`);
    console.log(`deps=${depsHash(locks)}`);
    console.log(`key=${rustcSlug(rustc)}-${depsHash(locks)}`);
    return 0;
  }
  if (cmd === "prune" && args.length === 2) {
    const names = pathPackages(readFileSync(args[0], "utf8"));
    const list = pruneList(args[1], names);
    let bytes = 0;
    for (const p of list) {
      bytes += duBytes(p);
      rmSync(p, { recursive: true, force: true });
    }
    console.log(
      `pruned ${list.length} entries (${(bytes / 2 ** 20).toFixed(0)} MiB) of ${names.length} path packages from ${args[1]}`,
    );
    return 0;
  }
  if (cmd === "tauri-cli" && args.length === 1) {
    const lock = JSON.parse(readFileSync(args[0], "utf8"));
    const v = lock.packages?.["node_modules/@tauri-apps/cli"]?.version;
    if (!v) throw new Error(`@tauri-apps/cli not in ${args[0]}`);
    console.log(`key=tauri-cli-${v}`);
    return 0;
  }
  console.error("usage: cargo-cache.mjs key <Cargo.lock>... | prune <Cargo.lock> <target> | tauri-cli <package-lock.json>");
  return 2;
}

if (import.meta.url === pathToFileURL(process.argv[1] || "").href) {
  process.exit(main(process.argv.slice(2)));
}
