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
 * Then loads `client/src/i18n/locales/en.ts`, finds every key declared
 * in the English dictionary, and reports which keys aren't referenced by
 * any .tsx/.ts file. With `--apply` it strips those dead keys from EVERY
 * per-locale file under `client/src/i18n/locales/*.ts` (en + all 17
 * others), shrinks the `i18n-known-missing.json` allowlist accordingly,
 * and exits.
 *
 * (Pre-2.x this read a single monolithic `client/src/i18n.ts` with all
 * languages inline; the repo since split to one file per locale. This
 * tool follows the same `vm`-extraction approach as i18n-coverage.mjs /
 * find-orphan-i18n.mjs.)
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
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const ALLOWLIST_PATH = path.join(repoRoot, "scripts/i18n-known-missing.json");
const SRC_ROOT = path.join(repoRoot, "client/src");

const apply = process.argv.includes("--apply");

// Key-prefix denylist: any key whose name starts with one of these is
// kept regardless of the static scan, because it's used via dynamic
// templating somewhere. Add to this list when a new dynamic-key
// pattern lands; over-keeping is safer than over-deleting.
//
// Dynamic-key audit history:
// - `tr(section.key, ...)` in Sidebar (NAV_ITEMS array) → nav_section_*
// - `tr(f.titleKey, ...)` / `tr(f.bodyKey, ...)` in About → about_feat_*
// - `tr(o.labelKey, ...)` in Search → search_size_*
// Run the script without --apply to see live tr(IDENT,…) sites; if a
// new pattern appears, find its resolved key prefix and add it here.
const DYNAMIC_PREFIXES = [
  "queue_strategy_",
  "playlist_status_",
  "log_level_",
  "language_",
  // Reconcile + transfer phase enum values get rendered via
  // `tr(\`reconcile_mode_${mode}\`)` in some screens.
  "reconcile_mode_",
  // Sidebar section headers: rendered via `tr(section.key, ...)` in
  // a constant array of `{ section: { key: "nav_section_X" } }`
  // entries. The static extractor can't follow object property
  // accesses, so the keys themselves must be allowlisted here.
  "nav_section_",
  // About-screen feature tiles: rendered via `tr(f.titleKey, ...)`
  // and `tr(f.bodyKey, ...)` over a feature-list constant.
  "about_feat_",
  // Search-screen size filter: rendered via `tr(o.labelKey, ...)`
  // over an options array.
  "search_size_",
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

/** Detect `tr(IDENTIFIER, …)` / `tr(expr.foo, …)` / `tr(condition ? a : b, …)`
 *  patterns where the first argument resolves to a runtime value
 *  the static extractor can't see — they could delete a live key.
 *
 *  Tightened regex: requires the first arg to start with an
 *  identifier-legal char (letter / underscore / dollar). This
 *  rejects multi-line static calls (`tr(\n  "string"`) where the
 *  next non-whitespace is a quote, and only flags actual identifier
 *  references. Function definitions (`function t(lang:`) get the
 *  word-boundary `\b` match too — caller filters by the i18n.ts
 *  exclusion implicit in walkSrc not visiting that file's tests. */
function extractIdentifierCalls(content) {
  const out = [];
  const re = /\btr\s*\(\s*([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)\s*[,?)]/g;
  let m;
  while ((m = re.exec(content)) !== null) {
    const line = content.slice(0, m.index).split("\n").length;
    out.push({ line, snippet: m[0].slice(0, 60).trim() });
  }
  return out;
}

/** Locate the `const <name>: Translations = { ... }` object literal in
 *  a per-locale file and return its source span. Mirrors the matcher in
 *  i18n-coverage.mjs / find-orphan-i18n.mjs so all the i18n tools agree
 *  on the file shape our generator emits. */
function locateLiteral(src, fp) {
  const startMatch = src.match(
    /^const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*/m,
  );
  if (!startMatch) {
    throw new Error(`${fp}: could not find 'const <name>: Translations = ' declaration`);
  }
  const startIdx = startMatch.index + startMatch[0].length;
  const endMatch = src.slice(startIdx).match(/\n};\s*\n\s*export default/);
  if (!endMatch) {
    throw new Error(`${fp}: could not find end of literal (};\\nexport default)`);
  }
  const literalSrc = src.slice(startIdx, startIdx + endMatch.index + "\n}".length);
  return { startIdx, endIdx: startIdx + endMatch.index + "\n}".length, literalSrc };
}

/** All `.ts` locale files under client/src/i18n/locales. */
function localeFiles() {
  if (!fs.existsSync(LOCALES_DIR)) {
    throw new Error(
      `i18n locales dir not found: ${LOCALES_DIR} (expected one file per locale, e.g. en.ts)`,
    );
  }
  return fs
    .readdirSync(LOCALES_DIR)
    .filter((f) => f.endsWith(".ts") && !f.endsWith(".d.ts"))
    .map((f) => path.join(LOCALES_DIR, f));
}

function loadEnKeys() {
  const fp = path.join(LOCALES_DIR, "en.ts");
  if (!fs.existsSync(fp)) {
    throw new Error(`locales dir is missing en.ts (English is required): ${fp}`);
  }
  const { literalSrc } = locateLiteral(fs.readFileSync(fp, "utf8"), fp);
  const dict = vm.runInNewContext(`(${literalSrc})`, Object.create(null), {
    timeout: 1000,
  });
  return new Set(Object.keys(dict));
}

const usedKeys = new Set();
const identifierCalls = []; // [{file, line, snippet}] of tr(IDENT, …) sites
for (const f of walkSrc(SRC_ROOT)) {
  const content = fs.readFileSync(f, "utf8");
  for (const k of extractKeysFrom(content)) usedKeys.add(k);
  for (const site of extractIdentifierCalls(content)) {
    identifierCalls.push({ file: path.relative(repoRoot, f), ...site });
  }
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
  `[i18n-prune] used=${usedKeys.size} en=${enKeys.size} dead=${dead.length} phantom=${phantomKeys.length} dynamic=${identifierCalls.length}\n`,
);

// Surface dynamic key call sites — `tr(SOME_IDENT, …)` patterns
// the static extractor can't resolve. The pruner CAN'T know what
// runtime value those identifiers hold; without listing them, an
// operator running --apply could silently nuke a key referenced
// only via an identifier or ternary. Print up to 20 sites, then
// require the operator to either:
//   - convert the dynamic call to a static literal
//   - add the resolved key prefix to DYNAMIC_PREFIXES
// before the prune is safe.
if (identifierCalls.length > 0) {
  process.stdout.write(`[i18n-prune] tr(IDENTIFIER, …) sites detected (first 20):\n`);
  for (const site of identifierCalls.slice(0, 20)) {
    process.stdout.write(`  ${site.file}:${site.line} → ${site.snippet}\n`);
  }
  if (identifierCalls.length > 20) {
    process.stdout.write(`  … and ${identifierCalls.length - 20} more\n`);
  }
  if (apply) {
    process.stderr.write(
      `\n[i18n-prune] refusing --apply: ${identifierCalls.length} dynamic-key call site(s) above could resolve to keys that the prune would delete. Either convert them to static literals or add the resolved prefix to DYNAMIC_PREFIXES, then re-run.\n`,
    );
    process.exit(1);
  }
}

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

// ── Apply: strip dead keys from every per-locale file ──────────────
//
// Each client/src/i18n/locales/<code>.ts holds one language's object
// with many `key: "value"` lines. The keys we delete are the same set
// across every locale. A single regex per dead-key, applied globally
// to each file, strips the matching line wherever it appears.
//
// We match the entire line (leading whitespace + key + colon + value
// + trailing comma + newline) so the source stays well-formatted after
// pruning. Multi-line string values are not currently used in the
// locale files; if they ever are, this regex needs a more careful
// matcher. We only touch the bytes inside the object literal so a key
// name that happens to collide with text in the file header/footer
// can't be clobbered.
let before = 0;
let after = 0;
let stripped = 0;
const deadRes = dead.map(
  (k) => new RegExp(`^\\s+${escapeRegex(k)}\\s*:\\s*"(?:\\\\.|[^"\\\\])*"\\s*,?\\s*\\n`, "gm"),
);
for (const fp of localeFiles()) {
  const orig = fs.readFileSync(fp, "utf8");
  const { startIdx, endIdx } = locateLiteral(orig, fp);
  const head = orig.slice(0, startIdx);
  let body = orig.slice(startIdx, endIdx);
  const tail = orig.slice(endIdx);
  before += orig.length;
  for (const re of deadRes) {
    body = body.replace(re, () => {
      stripped++;
      return "";
    });
  }
  const next = head + body + tail;
  after += next.length;
  if (next !== orig) fs.writeFileSync(fp, next, "utf8");
}

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
  `[i18n-prune] stripped ${stripped} key-lines (${before - after} bytes) across locale files and shrunk allowlist by ${allowlistShrunk} entries.\n`,
);

function escapeRegex(s) {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
