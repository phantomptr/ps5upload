#!/usr/bin/env node
/*
 * i18n coverage gate.
 *
 * Loads `client/src/i18n/locales/*.ts` and compares every non-English
 * language's key set against English. For each non-en language:
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
 * It also fails on source drift: English rewritten while a translation
 * stayed as it was (see scripts/i18n-source-hashes.mjs).
 *
 * Modes
 *   default          Check; non-zero if any non-allowlisted miss.
 *   --report         Print per-language miss counts even when passing.
 *   --strict         Also fail on allowlist entries that are no longer
 *                    needed (validate-repo.mjs runs this). Without it they
 *                    are only printed.
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
import { fileURLToPath } from "node:url";
import { duplicateKeys, scanLocale } from "./i18n-locale-file.mjs";
import { checkSourceHashes, describeProblems } from "./i18n-source-hashes.mjs";
import { usedKeysIn } from "./i18n-usage.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const ALLOWLIST_PATH = path.join(repoRoot, "scripts/i18n-known-missing.json");

const args = process.argv.slice(2);
const updateAllowlist = args.includes("--update-allowlist");
const report = args.includes("--report");
const strict = args.includes("--strict");

/**
 * Load translations from the per-locale split layout under
 * `client/src/i18n/locales/<code>.ts`.
 *
 * Each locale file is plain TS with one runtime surface: a top-level
 * `const <safeId>: Translations = { ... }` object literal. The filename
 * keeps the canonical hyphenated code (`zh-CN.ts`); the identifier inside
 * (`zh_CN`) is internal. scripts/i18n-locale-file.mjs tokenizes the literal,
 * so Node needs no TS loader and text inside a value can never look like a
 * key.
 *
 * Duplicate-key guard: a repeated key in a locale literal is valid JS (the
 * later one silently wins), so it would quietly override a translation.
 */
function loadTranslations() {
  if (!fs.existsSync(LOCALES_DIR)) {
    throw new Error(
      `i18n locales dir not found: ${LOCALES_DIR} ` +
        "(expected one file per locale, e.g. en.ts, zh-CN.ts, …)",
    );
  }
  const files = fs
    .readdirSync(LOCALES_DIR)
    .filter((f) => f.endsWith(".ts") && !f.endsWith(".d.ts"));
  const translations = {};
  for (const f of files) {
    const code = f.replace(/\.ts$/, "");
    const fp = path.join(LOCALES_DIR, f);
    let entries;
    try {
      ({ entries } = scanLocale(fs.readFileSync(fp, "utf8")));
    } catch (e) {
      throw new Error(`${fp}: ${e.message}`);
    }
    const dups = duplicateKeys(entries);
    if (dups.length > 0) {
      throw new Error(
        `${fp}: duplicate locale key(s) — a dup silently overrides a ` +
          `translation: ${dups.join(", ")}`,
      );
    }
    const dict = {};
    for (const e of entries) dict[e.key] = e.value;
    translations[code] = dict;
  }
  if (!translations.en) {
    throw new Error("locales dir is missing en.ts (English is required)");
  }
  return translations;
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
  if (Array.isArray(entry)) return { missing: entry, stale: [], untranslated: [] };
  return {
    missing: entry?.missing ?? [],
    stale: entry?.stale ?? [],
    untranslated: entry?.untranslated ?? [],
  };
}

/**
 * Is this en value PROSE — i.e. would a translator be expected to change
 * it?
 *
 * Key presence alone is a bad proxy for "translated": a locale file can
 * carry every key in en.ts and still be byte-identical English for
 * hundreds of them. That is exactly what happened here — 18 locales
 * reported 100% coverage while ~220 keys each were untouched English,
 * because the gate below only ever compared key SETS.
 *
 * The heuristic: two or more ASCII words of 3+ letters means prose.
 * That exempts the large tail of values it is correct to leave alone —
 * units ("MB/s", "GHz"), acronyms ("CPU", "SoC", "M.2 SSD"), bare
 * interpolations ("{n}"), and protocol literals ("pxplay:"). Anything
 * the heuristic gets wrong (proper nouns like "PlayStation Store")
 * goes in the `untranslated` allowlist bucket, same as every other
 * exception this gate tracks.
 */
function isProse(value) {
  return (value.match(/[A-Za-z]{3,}/g) ?? []).length >= 2;
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
    const allowedUntranslated = new Set(entry.untranslated);
    const missing = [...enKeys]
      .filter((k) => !langKeys.has(k) && !allowedMissing.has(k))
      .sort();
    const stale = [...langKeys]
      .filter((k) => !enKeys.has(k) && !allowedStale.has(k))
      .sort();
    const untranslated = [...enKeys]
      .filter(
        (k) =>
          langKeys.has(k) &&
          dict[k] === en[k] &&
          isProse(en[k]) &&
          !allowedUntranslated.has(k),
      )
      .sort();
    result[lang] = { missing, stale, untranslated };
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
    const untranslated = [...enKeys]
      .filter(
        (k) =>
          langKeys.has(k) &&
          dict[k] === translations.en[k] &&
          isProse(translations.en[k]),
      )
      .sort();
    if (missing.length > 0 || stale.length > 0 || untranslated.length > 0) {
      result[lang] = { missing, stale, untranslated };
    }
  }
  return result;
}

/**
 * Scan `client/src` for i18n keys referenced via `tr("literal", …)`,
 * `trStatic("literal", …)`, the console page's `t("literal", …)` and
 * `t(lang, "literal", …)`, and return the set of those NOT present in en.ts
 * (the console page's strings are generated from en.ts's `cw_*` keys).
 *
 * Why this matters: the per-language coverage check above only validates
 * translations OF en.ts keys. A key used at a call site with an inline
 * fallback — `tr("foo", undefined, "Foo")` — but never added to en.ts is
 * INVISIBLE to that check, yet it renders the English fallback for every
 * user (including non-English locales) and can never be translated. This
 * catches that class (fixed a batch of 70 such keys in the PR that added
 * this guard). Multi-line `tr(\n  "key"…)` calls are matched too.
 *
 * Only LITERAL keys are checked; dynamically-built keys
 * (`tr(\`x.${k}\`)`) can't be statically resolved and are skipped.
 * `foo`/`x` are the doc-comment example keys in i18n.ts / lang.ts and are
 * ignored. Tests are skipped: they call `t("ja", …)` with a language first.
 */
function findUsedButMissing(enKeys) {
  const SRC_DIR = path.join(repoRoot, "client/src");
  const IGNORE = new Set(["foo", "x"]);
  const used = new Set();
  const walk = (dir) => {
    for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
      const fp = path.join(dir, ent.name);
      if (ent.isDirectory()) {
        if (ent.name === "i18n" && dir.endsWith("/src")) continue; // skip locale defs
        walk(fp);
      } else if (/\.tsx?$/.test(ent.name) && !/\.test\./.test(ent.name)) {
        for (const k of usedKeysIn(fs.readFileSync(fp, "utf8"))) used.add(k);
      }
    }
  };
  walk(SRC_DIR);
  return [...used]
    .filter((k) => !IGNORE.has(k) && !enKeys.has(k))
    .sort();
}

/**
 * Allowlist entries that no longer describe anything: an "untranslated" key
 * that now has a translation, a "missing" key that is now there, a "stale"
 * key that is gone. Nothing else ever removes them, so without this the
 * register only grows and stops being the tight list it is meant to be.
 */
function findNeedlessAllowlist(translations, allowlist) {
  const en = translations.en;
  const out = [];
  for (const [lang, raw] of Object.entries(allowlist)) {
    const dict = translations[lang] ?? {};
    const entry = readEntry(raw);
    for (const k of entry.untranslated) {
      if (dict[k] !== en[k] || en[k] === undefined) out.push(`${lang}: untranslated ${k}`);
    }
    for (const k of entry.missing) if (k in dict || !(k in en)) out.push(`${lang}: missing ${k}`);
    for (const k of entry.stale) if (!(k in dict) || k in en) out.push(`${lang}: stale ${k}`);
  }
  return out.sort();
}

const translations = loadTranslations();

// Gate: any key USED at a call site but absent from en.ts. Runs before the
// per-language coverage check (and is skipped in --update-allowlist bootstrap).
if (!updateAllowlist) {
  const enKeys = new Set(Object.keys(translations.en ?? {}));
  const usedButMissing = findUsedButMissing(enKeys);
  if (usedButMissing.length > 0) {
    process.stderr.write(
      `[i18n-coverage] failed: ${usedButMissing.length} key(s) used via tr("…") but NOT in en.ts.\n` +
        "These render their English fallback for EVERY locale and can never be translated.\n" +
        "Add each to client/src/i18n/locales/en.ts (then allowlist for other locales):\n",
    );
    for (const k of usedButMissing) process.stderr.write(`      ${k}\n`);
    process.exit(1);
  }
}

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

let coverageFailed = false;
const lines = [];
for (const [lang, info] of Object.entries(perLang)) {
  const missCount = info.missing.length;
  const staleCount = info.stale.length;
  const untransCount = info.untranslated.length;
  const entry = readEntry(allowlist[lang]);
  const allowCount =
    entry.missing.length + entry.stale.length + entry.untranslated.length;
  if (missCount === 0 && staleCount === 0 && untransCount === 0) {
    if (report) {
      lines.push(
        `  ${lang.padEnd(8)} ok           (${allowCount} allowlisted)`,
      );
    }
    continue;
  }
  coverageFailed = true;
  lines.push(
    `  ${lang.padEnd(8)} missing=${missCount} stale=${staleCount} ` +
      `untranslated=${untransCount} allowlisted=${allowCount}`,
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
  for (const k of info.untranslated.slice(0, 20)) {
    lines.push(`      untranslated (still English): ${k}`);
  }
  if (info.untranslated.length > 20) {
    lines.push(`      … and ${info.untranslated.length - 20} more`);
  }
}

if (coverageFailed) {
  process.stderr.write("[i18n-coverage] failed:\n");
  process.stderr.write(`${lines.join("\n")}\n`);
  process.stderr.write(
    "\n  fix: translate the keys above OR add them to scripts/i18n-known-missing.json\n",
  );
  process.stderr.write(
    "  bootstrap (caution): node scripts/i18n-coverage.mjs --update-allowlist\n",
  );
}

const needless = findNeedlessAllowlist(translations, allowlist);
if (needless.length > 0) {
  process.stderr.write(
    `[i18n-coverage] ${needless.length} entries in scripts/i18n-known-missing.json are no longer needed; remove them:\n`,
  );
  for (const n of needless) process.stderr.write(`      ${n}\n`);
}

const drift = describeProblems(checkSourceHashes(repoRoot));
if (drift) process.stderr.write(`[i18n-coverage] source drift:\n${drift}\n`);

if (coverageFailed || drift || (strict && needless.length > 0)) process.exit(1);

if (report) {
  process.stdout.write("[i18n-coverage] all languages within allowlist budget\n");
  process.stdout.write(`${lines.join("\n")}\n`);
} else {
  process.stdout.write(
    `[i18n-coverage] ok (${Object.keys(translations).length} languages)\n`,
  );
}
