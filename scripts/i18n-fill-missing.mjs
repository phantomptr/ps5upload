#!/usr/bin/env node
/*
 * i18n hole-filler.
 *
 * Two-phase fix for the "phantom keys" problem — keys used in code
 * but absent from a locale dictionary:
 *
 *  Phase 1: Extract `tr("key", vars, "fallback")` calls from every
 *  .tsx/.ts file in client/src. Any key that's used in code but
 *  missing from `client/src/i18n/locales/en.ts` gets appended to en
 *  with its fallback string as the value.
 *
 *  Phase 2: For every other locale file under
 *  `client/src/i18n/locales/`, find the keys present in en but
 *  missing locally. Translate each via the MyMemory free API (no
 *  auth needed for ~5k chars/day per IP), then append into the
 *  locale file.
 *
 * Idempotent: re-runs are safe — phase 1 skips keys already in en;
 * phase 2 skips keys already in the target locale.
 *
 * Locale-split aware: the old monolithic `client/src/i18n.ts` with
 * nested `en: {}` / `"zh-CN": {}` blocks is gone. Each locale is now
 * its own file (`en.ts`, `fr.ts`, `zh-CN.ts`, …) shaped:
 *
 *     const <id>: Translations = {
 *       key: "value",
 *       ...
 *     };
 *     export default <id>;
 *
 * After running this, re-run `node scripts/i18n-coverage.mjs` to
 * confirm the gate is satisfied (or to refresh the allowlist).
 *
 * Modes:
 *   default    Run phase 1 + phase 2 against MyMemory.
 *   --dry-run  Print what would change; don't touch files.
 *   --en-only  Only run phase 1 (add phantoms to en); skip
 *              translations. Useful when offline or rate-limited.
 */

import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const EN_PATH = path.join(LOCALES_DIR, "en.ts");
const SRC_ROOT = path.join(repoRoot, "client/src");

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const enOnly = args.includes("--en-only");

const MYMEMORY_URL = "https://api.mymemory.translated.net/get";
const TRANSLATE_SLEEP_MS = 250;

// ── locale-file IO ─────────────────────────────────────────────

/** Extract + evaluate the `const <id>: Translations = { ... }`
 *  literal from a locale file. Mirrors scripts/i18n-coverage.mjs. */
function parseLocaleFile(fp) {
  const src = fs.readFileSync(fp, "utf8");
  const startMatch = src.match(
    /^const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*/m,
  );
  if (!startMatch) {
    throw new Error(`${fp}: could not find 'const <name>: Translations =' declaration`);
  }
  const startIdx = startMatch.index + startMatch[0].length;
  const endMatch = src.slice(startIdx).match(/\n};\s*\n\s*export default/);
  if (!endMatch) {
    throw new Error(`${fp}: could not find end of literal (};\\nexport default)`);
  }
  // Absolute index of the `\n};` that closes the object literal.
  const literalCloseIdx = startIdx + endMatch.index;
  const literalSrc = src.slice(startIdx, literalCloseIdx + "\n}".length);
  const dict = vm.runInNewContext(`(${literalSrc})`, Object.create(null), {
    timeout: 1000,
  });
  return { src, dict, literalCloseIdx };
}

function escapeTs(s) {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
}

/** Render an object-literal key. Bare identifiers are emitted as-is;
 *  anything else (dotted keys like `install.phase.staging`, hyphenated
 *  locale-ish keys) must be quoted to stay valid TS. */
function renderKey(key) {
  return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(key)
    ? key
    : `"${escapeTs(key)}"`;
}

/** Append `additions` (array of {key,value}) to a locale file just
 *  before the closing `\n};`. The last existing key line is
 *  normalised to end with a comma first — locale files in this repo
 *  are inconsistent about the trailing comma on the final entry. */
function appendKeys(fp, additions) {
  if (additions.length === 0) return;
  const { src, literalCloseIdx } = parseLocaleFile(fp);
  let before = src.slice(0, literalCloseIdx);
  const after = src.slice(literalCloseIdx); // starts with "\n};"
  // Ensure the last non-whitespace char before `\n};` is a comma so
  // appending another entry stays valid JS.
  const trimmed = before.replace(/\s*$/, "");
  before = /[,{]$/.test(trimmed) ? trimmed : `${trimmed},`;
  const block = additions
    .map((a) => `${renderKey(a.key)}: "${escapeTs(a.value)}",`)
    .join("\n");
  fs.writeFileSync(fp, `${before}\n${block}${after}`, "utf8");
}

// ── Phase 1: extract phantom (key, fallback) from TSX ──────────

function walkSrc(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkSrc(full));
    else if (/\.(tsx|ts)$/i.test(entry.name) && !entry.name.endsWith(".test.ts")) {
      out.push(full);
    }
  }
  return out;
}

/** Strip `/* … *‍/` blocks and whole-line `//` comments. Keeps the
 *  phantom scanner from picking up `tr("foo")` examples that live in
 *  JSDoc (e.g. the usage block in state/lang.ts). Whole-line only, so
 *  a `//` inside a string literal (URLs) is left intact. */
function stripComments(content) {
  return content
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/^\s*\/\/.*$/gm, "");
}

/** Extract `tr("key", vars, "fallback")` and `tr("key", "fallback")`
 *  patterns. Returns Map<key, fallback>.
 *  - The 3-arg form has a vars argument (object literal / undefined)
 *    between key and fallback; the 2-arg form has the fallback
 *    immediately after the key.
 *  - Only static string-literal fallbacks are captured (template
 *    literals are dynamic by nature — can't pre-translate).
 *  - Keys containing whitespace are rejected — a real i18n key is
 *    snake_case or dotted; a "key" with a space is a mis-parse.
 *  - ALSO harvests data-table pairs: object literals with sibling
 *    `key: "x"` + `fallback: "y"` string-literal properties. This is
 *    how the sidebar nav, command palette, and other config-driven
 *    UIs feed `tr(item.key, item.fallback)` at render time; without
 *    this branch the keys never land in en.ts so every locale shows
 *    the English fallback. The two property names must be SIBLINGS
 *    in the same object literal — the regex matches them in either
 *    order, separated by zero or more other props on the same line
 *    or adjacent lines, all within one balanced `{ ... }`. */
function extractPhantoms(rawContent) {
  const content = stripComments(rawContent);
  const map = new Map();
  // The trailing `,?\s*` before `\)` matters: multi-line tr() calls
  // are commonly written with a trailing comma —
  //   tr(
  //     "key",
  //     "fallback",
  //   )
  // — and without allowing that comma the whole call fails to match.
  // 3-arg: tr("key", <vars>, "fallback")
  const re3 =
    /\b(?:tr|t)\s*\(\s*(["'])([^"'\\\n]+?)\1\s*,\s*[^)]*?,\s*(["'])((?:\\.|[^\\\n])*?)\3\s*,?\s*\)/g;
  // 2-arg: tr("key", "fallback")
  const re2 =
    /\b(?:tr|t)\s*\(\s*(["'])([^"'\\\n]+?)\1\s*,\s*(["'])((?:\\.|[^\\\n])*?)\3\s*,?\s*\)/g;
  for (const re of [re3, re2]) {
    let m;
    while ((m = re.exec(content)) !== null) {
      const key = m[2];
      const fallback = m[4];
      if (/\s/.test(key)) continue; // mis-parse, not a real key
      if (!map.has(key)) map.set(key, fallback);
    }
  }
  // Data-table form. Two flavours:
  //   { ..., key: "x", ..., fallback: "y", ... }   (key first)
  //   { ..., fallback: "y", ..., key: "x", ... }   (fallback first)
  // The `[^{}]*?` window keeps each match inside ONE object literal —
  // a nested brace pair would force the regex past `}` and fail.
  // Whitespace key check still applies.
  const dataA =
    /\bkey\s*:\s*(["'])([A-Za-z0-9_.$-]+)\1[^{}]*?,\s*fallback\s*:\s*(["'])((?:\\.|[^\\\n])*?)\3/g;
  const dataB =
    /\bfallback\s*:\s*(["'])((?:\\.|[^\\\n])*?)\1[^{}]*?,\s*key\s*:\s*(["'])([A-Za-z0-9_.$-]+)\3/g;
  let m;
  while ((m = dataA.exec(content)) !== null) {
    const key = m[2];
    const fallback = m[4];
    if (/\s/.test(key)) continue;
    if (!map.has(key)) map.set(key, fallback);
  }
  while ((m = dataB.exec(content)) !== null) {
    const key = m[4];
    const fallback = m[2];
    if (/\s/.test(key)) continue;
    if (!map.has(key)) map.set(key, fallback);
  }
  return map;
}

const tsFiles = walkSrc(SRC_ROOT).filter(
  (f) => !f.startsWith(LOCALES_DIR + path.sep),
);
const phantomMap = new Map();
for (const f of tsFiles) {
  for (const [k, v] of extractPhantoms(fs.readFileSync(f, "utf8"))) {
    if (!phantomMap.has(k)) phantomMap.set(k, v);
  }
}

const enParsed = parseLocaleFile(EN_PATH);
const enKeys = new Set(Object.keys(enParsed.dict));
const enMissing = [...phantomMap.entries()].filter(([k]) => !enKeys.has(k));

process.stdout.write(
  `[i18n-fill] phantom (used in code, missing from en): ${enMissing.length}\n`,
);

if (enMissing.length > 0) {
  if (!dryRun) {
    appendKeys(
      EN_PATH,
      enMissing.map(([key, value]) => ({ key, value })),
    );
    process.stdout.write(`[i18n-fill] added ${enMissing.length} keys to en.\n`);
  } else {
    process.stdout.write(`[i18n-fill] would add to en (first 10):\n`);
    for (const [k] of enMissing.slice(0, 10)) {
      process.stdout.write(`  ${k}\n`);
    }
  }
}

if (enOnly) {
  process.stdout.write(`[i18n-fill] --en-only: skipping translations.\n`);
  process.exit(0);
}

// ── Phase 2: translate en → other locales via MyMemory ─────────

// Re-read en after phase 1 so freshly-added phantoms get translated.
const enValues = parseLocaleFile(EN_PATH).dict;
const enKeysFresh = Object.keys(enValues);

const targetLocales = fs
  .readdirSync(LOCALES_DIR)
  .filter((f) => f.endsWith(".ts") && !f.endsWith(".d.ts") && f !== "en.ts")
  .map((f) => f.replace(/\.ts$/, ""));

async function translateOne(text, target) {
  const url = `${MYMEMORY_URL}?q=${encodeURIComponent(text)}&langpair=en|${encodeURIComponent(target)}`;
  const res = await fetch(url);
  if (!res.ok) throw new Error(`mymemory ${res.status}`);
  const json = await res.json();
  const t = json?.responseData?.translatedText;
  if (typeof t !== "string" || t.length === 0) {
    throw new Error(`mymemory returned no translation: ${JSON.stringify(json).slice(0, 100)}`);
  }
  if (/MYMEMORY/i.test(t) && /WARNING|LIMIT|EXCEEDED/i.test(t)) {
    throw new Error(`mymemory quota: ${t}`);
  }
  return t;
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

/** Protect `{placeholder}` tokens from the translation engine so a
 *  remote MT pass can't mangle `{count}` into `{ compte }` etc. */
function protectPlaceholders(text) {
  const placeholders = [];
  const protectedText = text.replace(/\{\w+\}/g, (m) => {
    placeholders.push(m);
    return `__VAR${placeholders.length - 1}__`;
  });
  return { protectedText, placeholders };
}

function restorePlaceholders(text, placeholders) {
  let out = text;
  placeholders.forEach((ph, i) => {
    out = out.replace(`__VAR${i}__`, ph);
  });
  return out;
}

async function fillLocale(lang) {
  const fp = path.join(LOCALES_DIR, `${lang}.ts`);
  const { dict } = parseLocaleFile(fp);
  const localKeys = new Set(Object.keys(dict));
  const missing = enKeysFresh.filter((k) => !localKeys.has(k));
  if (missing.length === 0) return { added: 0, failed: 0 };
  process.stdout.write(`[i18n-fill] ${lang}: ${missing.length} missing\n`);
  if (dryRun) return { added: 0, failed: 0 };

  const additions = [];
  let failed = 0;
  let quotaHit = false;
  for (const k of missing) {
    const enValue = enValues[k] ?? k;
    const { protectedText, placeholders } = protectPlaceholders(enValue);
    try {
      const translated = await translateOne(protectedText, lang);
      additions.push({ key: k, value: restorePlaceholders(translated, placeholders) });
    } catch (e) {
      process.stderr.write(`  ${lang}/${k}: ${e.message}\n`);
      failed++;
      if (String(e.message).includes("quota")) {
        quotaHit = true;
        break;
      }
    }
    await sleep(TRANSLATE_SLEEP_MS);
  }
  appendKeys(fp, additions);
  return { added: additions.length, failed, quotaHit };
}

(async () => {
  let totalAdded = 0;
  let totalFailed = 0;
  for (const lang of targetLocales) {
    const r = await fillLocale(lang);
    totalAdded += r.added;
    totalFailed += r.failed;
    if (r.quotaHit) {
      process.stderr.write(`[i18n-fill] daily quota exhausted; stopping run\n`);
      break;
    }
  }
  process.stdout.write(
    `[i18n-fill] done: added=${totalAdded} failed=${totalFailed}\n`,
  );
})();
