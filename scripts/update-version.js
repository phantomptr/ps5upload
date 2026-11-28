#!/usr/bin/env node
/*
 * Version-sync tool. The VERSION file at the repo root is the single
 * source of truth for the app version. Every build artifact (payload,
 * desktop binary, npm package, Tauri bundle) derives its version from
 * this one file via the script below.
 *
 * Usage
 *   node scripts/update-version.js              Read VERSION, sync downstream files
 *   node scripts/update-version.js <x.y.z>      Write x.y.z to VERSION, then sync
 *   node scripts/update-version.js --check      Exit non-zero if any downstream file
 *                                               disagrees with VERSION
 *
 * Downstream targets
 *   - client/package.json                (field: version)
 *   - client/package-lock.json           (field: version + packages[""]  .version)
 *   - client/src-tauri/tauri.conf.json   (field: version)
 *   - client/src-tauri/Cargo.toml        (line: `version = "..."` under [package])
 *   - payload/include/config.h           (macro: PS5UPLOAD2_VERSION)
 *   - engine/Cargo.lock,
 *     client/src-tauri/Cargo.lock      (version of each local workspace crate)
 *   - engine/Cargo.toml                  (line: `version = "..."` under
 *                                         [workspace.package]; all engine
 *                                         crates inherit it via
 *                                         `version.workspace = true`)
 *   - apps/play/sce_sys/param.json       (field: contentVersion, in the PS5's
 *                                         fixed NN.NNN.NNN form: 13.0.0 is
 *                                         "13.000.000"). PS5Upload Play carries
 *                                         PS5Upload's version; only this one
 *                                         value is rewritten, the rest of the
 *                                         file is the app's own.
 *                                         scripts/check-play-version.mjs is the
 *                                         CI gate for it (and for a built zip).
 *
 * The engine reports this same version from `/api/version`, so it stays on
 * par with the client.
 */

const fs = require("fs");
const path = require("path");

const repoRoot = path.join(__dirname, "..");
const VERSION_FILE = path.join(repoRoot, "VERSION");

const args = process.argv.slice(2);
const checkMode = args.includes("--check");
const explicitVersion = args.find((a) => !a.startsWith("--"));

function fail(msg) {
  console.error(`update-version: ${msg}`);
  process.exit(1);
}

function readVersionFile() {
  if (!fs.existsSync(VERSION_FILE)) fail("VERSION file missing at repo root");
  return fs.readFileSync(VERSION_FILE, "utf8").trim();
}

function writeVersionFile(v) {
  fs.writeFileSync(VERSION_FILE, `${v}\n`);
}

function looksLikeSemver(v) {
  return /^\d+\.\d+\.\d+([-+][A-Za-z0-9._-]+)?$/.test(v);
}

// ─── Resolve target version ───────────────────────────────────────────────

let targetVersion;
if (explicitVersion) {
  if (!looksLikeSemver(explicitVersion))
    fail(`argument is not a semver: "${explicitVersion}"`);
  if (checkMode) fail("--check cannot be combined with an explicit version");
  targetVersion = explicitVersion;
  writeVersionFile(targetVersion);
  console.log(`VERSION → ${targetVersion}`);
} else {
  targetVersion = readVersionFile();
  if (!looksLikeSemver(targetVersion))
    fail(`VERSION file contains "${targetVersion}" — not a semver`);
}

// ─── Patchers: each returns { path, current, desired, ok } ────────────────

function patchJsonField(relPath, fieldPath) {
  const full = path.join(repoRoot, relPath);
  if (!fs.existsSync(full)) return null;
  const raw = fs.readFileSync(full, "utf8");
  const json = JSON.parse(raw);
  let current = json;
  for (const key of fieldPath) {
    if (current == null || typeof current !== "object") {
      current = undefined;
      break;
    }
    current = current[key];
  }
  if (typeof current !== "string") {
    return { path: relPath, current: "<missing>", desired: targetVersion, ok: false };
  }
  const ok = current === targetVersion;
  if (!ok && !checkMode) {
    // Mutate nested path in place.
    let cursor = json;
    for (let i = 0; i < fieldPath.length - 1; i++) cursor = cursor[fieldPath[i]];
    cursor[fieldPath[fieldPath.length - 1]] = targetVersion;
    // Preserve trailing newline if original had one.
    const trailing = raw.endsWith("\n") ? "\n" : "";
    fs.writeFileSync(full, JSON.stringify(json, null, 2) + trailing);
  }
  return { path: relPath, current, desired: targetVersion, ok };
}

function patchRegex(relPath, regex, replacer, extractor) {
  const full = path.join(repoRoot, relPath);
  if (!fs.existsSync(full)) return null;
  const raw = fs.readFileSync(full, "utf8");
  const m = raw.match(regex);
  const current = m ? extractor(m) : "<no-match>";
  const ok = current === targetVersion;
  if (!ok && !checkMode && m) {
    fs.writeFileSync(full, raw.replace(regex, replacer));
  }
  return { path: relPath, current, desired: targetVersion, ok };
}

/**
 * Patch the workspace crates' own versions inside a Cargo.lock.
 *
 * Cargo.lock records a version for every crate INCLUDING the local ones,
 * so bumping Cargo.toml alone leaves the lock stale. Nothing fails
 * loudly — cargo just silently rewrites the lock on the next build — so
 * the symptom is that a freshly released, freshly cloned tree is dirty
 * the moment anyone compiles it. That is what happened for v5.1.1.
 *
 * Local crates are exactly the `[[package]]` blocks with no `source`
 * key: registry and git dependencies always carry one, path members
 * never do. That rule needs no list of crate names to keep in sync.
 */
// Third-party crates we vendor and redirect to with `[patch.crates-io]`.
//
// They have no `source =` line in the lockfile (they resolve to a local
// path), which otherwise makes them look like one of our own crates and
// gets them "synced" to the ps5upload version — renaming unrar 0.5.8 to
// 5.3.1 and drifting the lockfile on every release. They keep upstream's
// version; only their vendored copy's own Cargo.toml governs it.
// AVA1 is versioned by its own repository the same way: `ava1` now comes from crates.io (it has a
// `source =` line), `ava1-chaos` is still a path dependency into the third_party/ava1 submodule.
const VENDORED_THIRD_PARTY = new Set(["unrar", "sevenz-rust2", "ava1", "ava1-chaos", "ava1-gen"]);

function patchCargoLock(relPath) {
  const full = path.join(repoRoot, relPath);
  if (!fs.existsSync(full)) return null;
  const raw = fs.readFileSync(full, "utf8");
  const blocks = raw.split(/(?=\[\[package\]\])/);
  let stale = 0;
  const patched = blocks.map((block) => {
    if (!block.startsWith("[[package]]")) return block;
    if (/^source = /m.test(block)) return block; // registry or git crate
    const nameMatch = block.match(/^name = "([^"]+)"/m);
    if (nameMatch && VENDORED_THIRD_PARTY.has(nameMatch[1])) return block;
    const m = block.match(/^version = "([^"]+)"/m);
    if (!m || m[1] === targetVersion) return block;
    stale++;
    return block.replace(/^version = "[^"]+"/m, `version = "${targetVersion}"`);
  });
  const ok = stale === 0;
  if (!ok && !checkMode) fs.writeFileSync(full, patched.join(""));
  return {
    path: relPath,
    current: ok ? targetVersion : `${stale} local crate(s) stale`,
    desired: targetVersion,
    ok,
  };
}

/**
 * The PS5's form of a version: `contentVersion` in a title's param.json is NN.NNN.NNN (13.0.0 is
 * "13.000.000", 12.6.44 is "12.006.044"). A pre-release or build suffix has no place there and is
 * left out. Null when a part does not fit (major over 99, minor or patch over 999).
 */
function ps5ContentVersion(v) {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(v);
  if (!m) return null;
  const [maj, min, pat] = [m[1], m[2], m[3]].map(Number);
  if (maj > 99 || min > 999 || pat > 999) return null;
  return `${String(maj).padStart(2, "0")}.${String(min).padStart(3, "0")}.${String(pat).padStart(3, "0")}`;
}

/**
 * PS5Upload Play's `contentVersion` (apps/play/sce_sys/param.json). Only that value is rewritten,
 * in place: the app owns the rest of the file and its formatting. Skipped (null) while the app is
 * not in the tree.
 */
function patchPlayContentVersion(relPath) {
  const full = path.join(repoRoot, relPath);
  if (!fs.existsSync(full)) return null;
  const desired = ps5ContentVersion(targetVersion);
  if (!desired) fail(`${targetVersion} has no PS5 contentVersion form (NN.NNN.NNN)`);
  const raw = fs.readFileSync(full, "utf8");
  const re = /("contentVersion"\s*:\s*")([^"]*)(")/;
  const m = raw.match(re);
  const current = m ? m[2] : "<no-match>";
  const ok = current === desired;
  if (!ok && !checkMode && m) {
    fs.writeFileSync(full, raw.replace(re, `$1${desired}$3`));
  }
  return { path: relPath, current, desired, ok };
}

// ─── Run all patchers ─────────────────────────────────────────────────────

const results = [
  patchJsonField("client/package.json", ["version"]),
  patchJsonField("client/package-lock.json", ["version"]),
  patchJsonField("client/package-lock.json", ["packages", "", "version"]),
  patchJsonField("client/src-tauri/tauri.conf.json", ["version"]),
  patchRegex(
    "client/src-tauri/Cargo.toml",
    /^version = "(\d+\.\d+\.\d+[^"]*)"/m,
    `version = "${targetVersion}"`,
    (m) => m[1],
  ),
  patchRegex(
    "payload/include/config.h",
    /^#define PS5UPLOAD2_VERSION "(\d+\.\d+\.\d+[^"]*)"/m,
    `#define PS5UPLOAD2_VERSION "${targetVersion}"`,
    (m) => m[1],
  ),
  // Engine workspace version. All engine crates inherit it via
  // `version.workspace = true`, so this one line drives the binary's
  // reported version (`/api/version`) and keeps the engine on par with
  // the client. The `^version = "x.y.z"` line lives under
  // `[workspace.package]`; dependency versions are inline (`serde = {
  // version = ... }`) and not line-anchored, so they're never matched.
  patchRegex(
    "engine/Cargo.toml",
    /^version = "(\d+\.\d+\.\d+[^"]*)"/m,
    `version = "${targetVersion}"`,
    (m) => m[1],
  ),
  patchCargoLock("engine/Cargo.lock"),
  patchCargoLock("client/src-tauri/Cargo.lock"),
  patchPlayContentVersion("apps/play/sce_sys/param.json"),
].filter(Boolean);

// ─── Report ───────────────────────────────────────────────────────────────

const pad = results.reduce((n, r) => Math.max(n, r.path.length), 0);
const drift = [];
// A pattern that found nothing to patch (`<no-match>`) or a JSON field that
// is not there (`<missing>`) was reported as "updated" and the run exited 0,
// so a reshuffled Cargo.toml or config.h would have kept its old version
// through a release. Nothing was written for these; they are failures.
const unpatchable = [];

for (const r of results) {
  const broken = r.current === "<no-match>" || r.current === "<missing>";
  const status = r.ok
    ? checkMode
      ? "ok"
      : "ok (unchanged)"
    : broken
      ? `ERROR (${r.current}: nothing to patch)`
      : checkMode
        ? `DRIFT (has ${r.current})`
        : `updated (was ${r.current})`;
  if (broken) unpatchable.push(r);
  else if (!r.ok) drift.push(r);
  console.log(`  ${r.path.padEnd(pad)}  ${status}`);
}

if (unpatchable.length > 0) {
  console.error("");
  console.error(
    `update-version: ${unpatchable.length} file(s) have no version line this script recognises: ${unpatchable.map((r) => r.path).join(", ")}.`,
  );
  console.error("Fix the file (or the pattern in scripts/update-version.js) before releasing.");
  process.exit(1);
}

if (checkMode && drift.length > 0) {
  console.error("");
  console.error(
    `update-version: ${drift.length} file(s) drifted from VERSION (${targetVersion}).`,
  );
  console.error("Run `node scripts/update-version.js` to sync.");
  process.exit(1);
}

if (checkMode) {
  console.log(`All files match VERSION (${targetVersion}).`);
} else if (drift.length === 0) {
  console.log(`All files already at ${targetVersion}; nothing to do.`);
} else {
  console.log(`Synced ${drift.length} file(s) to ${targetVersion}.`);
}
