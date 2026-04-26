#!/usr/bin/env node
/*
 * i18n coverage gate.
 *
 * Loads `client/src/i18n.ts` and compares every non-English language's
 * key set against English. For each non-en language:
 *
 *   missing = (en_keys − lang_keys) − allowlist[lang]
 *
 * If `missing` is non-empty for any language, the script exits non-
 * zero with a per-language report. The allowlist
 * (`scripts/i18n-known-missing.json`) is the curated "we know these
 * aren't translated yet" register — keep it tight so newly added keys
 * surface as build failures and force either a translation or an
 * explicit allowlist entry.
 *
 * Modes
 *   default          Check; non-zero if any non-allowlisted miss.
 *   --report         Print per-language miss counts even when passing.
 *   --update-allowlist
 *                    Rewrite allowlist with whatever is currently
 *                    missing. Use sparingly — the whole point of the
 *                    gate is that humans look at adds. Useful only for
 *                    bootstrapping or after a deliberate big i18n
 *                    purge.
 *
 * Wires into validate-repo.mjs as a separate gate so a missing
 * translation fails the same `npm run validate` users run before
 * pushing — no separate ritual.
 */

import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const I18N_PATH = path.join(repoRoot, "client/src/i18n.ts");
const ALLOWLIST_PATH = path.join(repoRoot, "scripts/i18n-known-missing.json");

const args = process.argv.slice(2);
const updateAllowlist = args.includes("--update-allowlist");
const report = args.includes("--report");

/**
 * Load `client/src/i18n.ts` data block.
 *
 * The file is plain TS with no runtime imports — its only runtime
 * surface is the `const translations = { ... }` object literal. We
 * extract that literal as a string and evaluate it in an isolated
 * V8 context (vm.runInNewContext) so type aliases, function bodies,
 * and the `export default` line don't trip up the parser.
 *
 * Why vm rather than `new Function()` or `eval`: vm.runInNewContext
 * runs in a fresh global with no access to require, process, fs, or
 * the surrounding closures — the strongest sandbox Node ships
 * without spawning a subprocess. The input here is repo-controlled
 * source we ship to users anyway, so the threat model is "did
 * someone commit a malformed literal" rather than "did someone
 * inject code via the file" — but the isolation costs nothing.
 *
 * Why not `import("./i18n.ts")` directly: Node can't load .ts files
 * without a TS loader (tsx, ts-node) which we'd then need to install
 * as a script-only dep. Doing the extraction inline keeps the
 * dependency graph minimal.
 */
function loadTranslations() {
  const src = fs.readFileSync(I18N_PATH, "utf8");
  const startIdx = src.search(/^const translations\s*=\s*/m);
  if (startIdx < 0) {
    throw new Error("i18n.ts: could not find 'const translations =' declaration");
  }
  const cutoff = src.indexOf("export type LanguageCode");
  if (cutoff < 0) {
    throw new Error("i18n.ts: could not find 'export type LanguageCode' marker");
  }
  // Slice out just the literal assignment (cuts before the type alias
  // + helper function block at the end of the file).
  const literalSrc = src.slice(startIdx, cutoff)
    .replace(/^const translations\s*=\s*/, "");
  // Drop any trailing semicolon so we can wrap as `(\nliteral\n)`.
  const trimmed = literalSrc.trim().replace(/;\s*$/, "");
  return vm.runInNewContext(`(${trimmed})`, Object.create(null), {
    timeout: 1000,
  });
}

function loadAllowlist() {
  if (!fs.existsSync(ALLOWLIST_PATH)) return {};
  try {
    return JSON.parse(fs.readFileSync(ALLOWLIST_PATH, "utf8"));
  } catch (e) {
    throw new Error(`could not parse ${ALLOWLIST_PATH}: ${e.message}`);
  }
}

/** Allowlist entries can be either a flat array of missing keys
 *  (legacy bootstrap shape) or an object with explicit `missing` and
 *  `stale` arrays. The object form is what `--update-allowlist`
 *  produces; both are accepted on read so older allowlist files
 *  written by the bootstrap step keep working. */
function readEntry(entry) {
  if (Array.isArray(entry)) return { missing: entry, stale: [] };
  return {
    missing: entry?.missing ?? [],
    stale: entry?.stale ?? [],
  };
}

function computeMissingPerLang(translations, allowlist) {
  const en = translations.en;
  if (!en) throw new Error("i18n.ts: missing 'en' language");
  const enKeys = new Set(Object.keys(en));
  const result = {};
  for (const [lang, dict] of Object.entries(translations)) {
    if (lang === "en") continue;
    const langKeys = new Set(Object.keys(dict));
    const entry = readEntry(allowlist[lang]);
    const allowedMissing = new Set(entry.missing);
    const allowedStale = new Set(entry.stale);
    const missing = [...enKeys]
      .filter((k) => !langKeys.has(k) && !allowedMissing.has(k))
      .sort();
    const stale = [...langKeys]
      .filter((k) => !enKeys.has(k) && !allowedStale.has(k))
      .sort();
    result[lang] = { missing, stale };
  }
  return result;
}

function computeAllForLang(translations) {
  const enKeys = new Set(Object.keys(translations.en ?? {}));
  const result = {};
  for (const [lang, dict] of Object.entries(translations)) {
    if (lang === "en") continue;
    const langKeys = new Set(Object.keys(dict));
    const missing = [...enKeys].filter((k) => !langKeys.has(k)).sort();
    const stale = [...langKeys].filter((k) => !enKeys.has(k)).sort();
    if (missing.length > 0 || stale.length > 0) {
      result[lang] = { missing, stale };
    }
  }
  return result;
}

const translations = loadTranslations();

if (updateAllowlist) {
  const all = computeAllForLang(translations);
  // Keep langs + their key arrays sorted for stable diffs.
  const sorted = Object.fromEntries(
    Object.keys(all)
      .sort()
      .map((lang) => [lang, all[lang]]),
  );
  fs.writeFileSync(
    ALLOWLIST_PATH,
    `${JSON.stringify(sorted, null, 2)}\n`,
    "utf8",
  );
  process.stdout.write(
    `[i18n-coverage] wrote allowlist for ${Object.keys(sorted).length} language(s) to ${path.relative(repoRoot, ALLOWLIST_PATH)}\n`,
  );
  process.exit(0);
}

const allowlist = loadAllowlist();
const perLang = computeMissingPerLang(translations, allowlist);

let failed = false;
const lines = [];
for (const [lang, info] of Object.entries(perLang)) {
  const missCount = info.missing.length;
  const staleCount = info.stale.length;
  const entry = readEntry(allowlist[lang]);
  const allowCount = entry.missing.length + entry.stale.length;
  if (missCount === 0 && staleCount === 0) {
    if (report) {
      lines.push(
        `  ${lang.padEnd(8)} ok           (${allowCount} allowlisted)`,
      );
    }
    continue;
  }
  failed = true;
  lines.push(
    `  ${lang.padEnd(8)} missing=${missCount} stale=${staleCount} allowlisted=${allowCount}`,
  );
  for (const k of info.missing.slice(0, 20)) {
    lines.push(`      missing: ${k}`);
  }
  if (info.missing.length > 20) {
    lines.push(`      … and ${info.missing.length - 20} more`);
  }
  for (const k of info.stale.slice(0, 20)) {
    lines.push(`      stale:   ${k}`);
  }
  if (info.stale.length > 20) {
    lines.push(`      … and ${info.stale.length - 20} more`);
  }
}

if (failed) {
  process.stderr.write("[i18n-coverage] failed:\n");
  process.stderr.write(`${lines.join("\n")}\n`);
  process.stderr.write(
    "\n  fix: translate the keys above OR add them to scripts/i18n-known-missing.json\n",
  );
  process.stderr.write(
    "  bootstrap (caution): node scripts/i18n-coverage.mjs --update-allowlist\n",
  );
  process.exit(1);
}

if (report) {
  process.stdout.write("[i18n-coverage] all languages within allowlist budget\n");
  process.stdout.write(`${lines.join("\n")}\n`);
} else {
  process.stdout.write(
    `[i18n-coverage] ok (${Object.keys(translations).length} languages)\n`,
  );
}
