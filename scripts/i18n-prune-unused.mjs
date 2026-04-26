#!/usr/bin/env node
/*
 * i18n dead-key pruner.
 *
 * Walks every .tsx + .ts file in client/src and extracts the static
 * key argument from calls like:
 *   tr("foo", ...)
 *   t("foo", ...)
 *   useTr()("foo", ...)
 *
 * Then loads `client/src/i18n.ts`, finds every key declared in the
 * `en: {...}` block, and reports which keys aren't referenced by any
 * .tsx/.ts file. With `--apply` it strips those dead keys from EVERY
 * language block in i18n.ts (en + all 17 others), shrinks the
 * `i18n-known-missing.json` allowlist accordingly, and exits.
 *
 * Why static-only extraction:
 * - Dynamically-keyed `tr(\`status_${snap.status}\`, ...)` calls
 *   can't be detected without runtime tracing. We accept that some
 *   true-positives may be flagged dead; the operator is expected
 *   to spot-check before --apply, and known-dynamic key prefixes
 *   are listed in DYNAMIC_PREFIXES below to skip pruning anything
 *   that starts with one.
 *
 * Modes:
 *   default          List dead/used counts; print first N dead keys.
 *   --apply          Strip dead keys from i18n.ts + allowlist.
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const I18N_PATH = path.join(repoRoot, "client/src/i18n.ts");
const ALLOWLIST_PATH = path.join(repoRoot, "scripts/i18n-known-missing.json");
const SRC_ROOT = path.join(repoRoot, "client/src");

const apply = process.argv.includes("--apply");

// Key-prefix denylist: any key whose name starts with one of these is
// kept regardless of the static scan, because it's used via dynamic
// templating somewhere. Add to this list when a new dynamic-key
// pattern lands; over-keeping is safer than over-deleting.
const DYNAMIC_PREFIXES = [
  "queue_strategy_",
  "playlist_status_",
  "log_level_",
  "language_",
  // Reconcile + transfer phase enum values get rendered via
  // `tr(\`reconcile_mode_${mode}\`)` in some screens.
  "reconcile_mode_",
];

function walkSrc(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      out.push(...walkSrc(full));
    } else if (/\.(tsx|ts)$/i.test(entry.name) && !entry.name.endsWith(".test.ts")) {
      out.push(full);
    }
  }
  return out;
}

/** Static-key extractor. Captures the first string-literal arg of
 *  `tr(...)` and `t(...)` calls. Two passes:
 *  - double-quoted: tr("key", ...)
 *  - single-quoted: tr('key', ...)
 *  Backtick-quoted (template) keys are skipped — those carry
 *  interpolation by definition and we list known prefixes in
 *  DYNAMIC_PREFIXES instead. */
function extractKeysFrom(content) {
  const keys = new Set();
  const patterns = [
    /\b(?:tr|t)\s*\(\s*"((?:\\.|[^"])+)"/g,
    /\b(?:tr|t)\s*\(\s*'((?:\\.|[^'])+)'/g,
  ];
  for (const re of patterns) {
    let m;
    while ((m = re.exec(content)) !== null) {
      keys.add(m[1]);
    }
  }
  return keys;
}

function loadEnKeys() {
  const src = fs.readFileSync(I18N_PATH, "utf8");
  const start = src.indexOf("\n  en: {");
  if (start < 0) throw new Error("en block not found");
  let depth = 1;
  let i = start + "\n  en: {".length;
  while (i < src.length) {
    if (src[i] === "{") depth++;
    else if (src[i] === "}") {
      depth--;
      if (depth === 0) break;
    }
    i++;
  }
  if (depth !== 0) throw new Error("en block not closed");
  const block = src.slice(start, i);
  const keyRe = /^\s+([a-zA-Z0-9_]+)\s*:\s*"/gm;
  const out = new Set();
  let m;
  while ((m = keyRe.exec(block)) !== null) {
    out.add(m[1]);
  }
  return out;
}

const usedKeys = new Set();
for (const f of walkSrc(SRC_ROOT)) {
  const content = fs.readFileSync(f, "utf8");
  for (const k of extractKeysFrom(content)) usedKeys.add(k);
}

const enKeys = loadEnKeys();

const dead = [];
for (const k of enKeys) {
  if (usedKeys.has(k)) continue;
  if (DYNAMIC_PREFIXES.some((p) => k.startsWith(p))) continue;
  dead.push(k);
}
dead.sort();

const phantomKeys = [...usedKeys].filter((k) => !enKeys.has(k));
phantomKeys.sort();

process.stdout.write(
  `[i18n-prune] used=${usedKeys.size} en=${enKeys.size} dead=${dead.length} phantom=${phantomKeys.length}\n`,
);

if (phantomKeys.length > 0) {
  process.stdout.write(`[i18n-prune] keys used in code but missing from en (first 20):\n`);
  for (const k of phantomKeys.slice(0, 20)) process.stdout.write(`  ${k}\n`);
  if (phantomKeys.length > 20) {
    process.stdout.write(`  … and ${phantomKeys.length - 20} more\n`);
  }
}

if (!apply) {
  process.stdout.write(`[i18n-prune] dead keys (first 30):\n`);
  for (const k of dead.slice(0, 30)) process.stdout.write(`  ${k}\n`);
  if (dead.length > 30) process.stdout.write(`  … and ${dead.length - 30} more\n`);
  process.stdout.write(
    `\n[i18n-prune] re-run with --apply to strip them from every language block.\n`,
  );
  process.exit(0);
}

if (dead.length === 0) {
  process.stdout.write(`[i18n-prune] nothing to prune.\n`);
  process.exit(0);
}

// ── Apply: strip dead keys from i18n.ts ────────────────────────────
//
// i18n.ts has 18 language blocks; each block has many `key: "value"`
// lines. The keys we want to delete are scattered across those
// blocks. A single regex per dead-key, applied globally, strips the
// matching line from anywhere in the file.
//
// We match the entire line (leading whitespace + key + colon + value
// + trailing comma + newline) so the source stays well-formatted
// after pruning. Multi-line strings are not currently used in
// i18n.ts; if they ever are, this regex will need a more careful
// matcher.

let src = fs.readFileSync(I18N_PATH, "utf8");
const before = src.length;
let stripped = 0;
for (const k of dead) {
  // Match: leading whitespace, exact key, optional whitespace,
  // colon, the value (string literal possibly with escaped quotes),
  // optional trailing comma, newline.
  const re = new RegExp(`^\\s+${escapeRegex(k)}\\s*:\\s*"(?:\\\\.|[^"\\\\])*"\\s*,?\\s*\\n`, "gm");
  src = src.replace(re, () => {
    stripped++;
    return "";
  });
}
fs.writeFileSync(I18N_PATH, src, "utf8");

// ── Apply: shrink the allowlist ──────────────────────────────────
//
// Keys that were on the missing/stale list because they existed in
// en but not in some other lang are now gone from en too — drop
// from each lang's missing list. Stale entries persist (they're
// "lang has, en doesn't"; pruning en doesn't reduce that).
let allowlist = {};
if (fs.existsSync(ALLOWLIST_PATH)) {
  allowlist = JSON.parse(fs.readFileSync(ALLOWLIST_PATH, "utf8"));
}
const deadSet = new Set(dead);
let allowlistShrunk = 0;
for (const lang of Object.keys(allowlist)) {
  const entry = allowlist[lang];
  const missing = Array.isArray(entry) ? entry : entry.missing ?? [];
  const stale = Array.isArray(entry) ? [] : entry.stale ?? [];
  const newMissing = missing.filter((k) => !deadSet.has(k));
  allowlistShrunk += missing.length - newMissing.length;
  if (Array.isArray(entry)) {
    allowlist[lang] = newMissing;
  } else {
    allowlist[lang] = { missing: newMissing, stale };
  }
}
fs.writeFileSync(
  ALLOWLIST_PATH,
  `${JSON.stringify(allowlist, null, 2)}\n`,
  "utf8",
);

process.stdout.write(
  `[i18n-prune] stripped ${stripped} key-lines (${before - src.length} bytes) and shrunk allowlist by ${allowlistShrunk} entries.\n`,
);

function escapeRegex(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
