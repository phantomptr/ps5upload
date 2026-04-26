#!/usr/bin/env node
/*
 * i18n hole-filler.
 *
 * Two-phase fix for the "phantom keys" problem after the dead-key
 * prune:
 *
 *  Phase 1: Extract `tr("key", vars, "fallback")` calls from every
 *  .tsx/.ts file in client/src. Any key that's used in code but
 *  missing from the en block of i18n.ts gets appended to en with
 *  its fallback string as the value.
 *
 *  Phase 2: For every other language block (17 of them), find the
 *  keys present in en but missing locally. Translate each via the
 *  MyMemory free API (no auth needed for ~5k chars/day per IP),
 *  then insert into the lang block.
 *
 * Idempotent: re-runs are safe — phase 1 skips keys already in en;
 * phase 2 skips keys already in the target lang.
 *
 * Modes:
 *   default    Run phase 1 + phase 2 against MyMemory.
 *   --dry-run  Print what would change; don't touch files.
 *   --en-only  Only run phase 1 (add phantoms to en); skip
 *              translations. Useful when offline or rate-limited.
 */

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const I18N_PATH = path.join(repoRoot, "client/src/i18n.ts");
const SRC_ROOT = path.join(repoRoot, "client/src");

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const enOnly = args.includes("--en-only");

const MYMEMORY_URL = "https://api.mymemory.translated.net/get";
const TRANSLATE_SLEEP_MS = 250;

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

/** Extract `tr("key", vars, "fallback")` and `tr('key', vars, 'fallback')`
 *  patterns. Returns Map<key, fallback>.
 *  - vars argument can be `undefined` or an object literal — matched
 *    lazily up to the next comma at the same depth.
 *  - Only static string-literal fallbacks captured (template literals
 *    skipped — those are dynamic by nature, can't pre-translate). */
function extractPhantoms(content) {
  const map = new Map();
  const re =
    /\b(?:tr|t)\s*\(\s*(["'])([^"'\\\n]+?)\1\s*,\s*[^)]*?,\s*(["'])((?:\\.|[^\\\n])*?)\3\s*\)/g;
  let m;
  while ((m = re.exec(content)) !== null) {
    const key = m[2];
    const fallback = m[4];
    if (!map.has(key)) map.set(key, fallback);
  }
  return map;
}

function loadEnBlock(src) {
  const start = src.indexOf("\n  en: {");
  if (start < 0) throw new Error("en block not found");
  let depth = 1;
  let i = start + "\n  en: {".length;
  // Same string-literal skipping as loadLangBlocks — a `{` or `}`
  // inside a key's value would otherwise corrupt the depth tracker
  // and the function would return blockEnd pointing INTO a string
  // literal. Inserting at that index then writes mid-string,
  // wrecking the file.
  let inString = null;
  while (i < src.length) {
    const ch = src[i];
    if (inString) {
      if (ch === "\\") {
        i += 2;
        continue;
      }
      if (ch === inString) inString = null;
      i++;
      continue;
    }
    if (ch === '"' || ch === "'") {
      inString = ch;
      i++;
      continue;
    }
    if (ch === "{") depth++;
    else if (ch === "}") {
      depth--;
      if (depth === 0) break;
    }
    i++;
  }
  if (depth !== 0) throw new Error("en block not closed");
  const block = src.slice(start, i);
  const keyRe = /^\s+([a-zA-Z0-9_]+)\s*:\s*"((?:\\.|[^"\\])*)"\s*,?/gm;
  const keys = new Set();
  const values = new Map();
  let m;
  while ((m = keyRe.exec(block)) !== null) {
    keys.add(m[1]);
    values.set(m[1], m[2].replace(/\\"/g, '"').replace(/\\\\/g, "\\"));
  }
  return { keys, values, blockEnd: i };
}

function loadLangBlocks(src) {
  const out = {};
  // Match both bare-key (`fr: {`) and quoted-key (`"zh-CN": {`)
  // forms — the latter is required because `pt-BR` / `zh-CN` /
  // `zh-TW` are not bare-identifier-legal in JS object literals
  // and must be quoted. The original regex only matched the bare
  // form and silently skipped the three hyphenated langs, so the
  // first translate run only filled fr/es/ar.
  const langRe = /\n  "?([a-zA-Z][a-zA-Z-]*)"?:\s*\{/g;
  let m;
  while ((m = langRe.exec(src)) !== null) {
    const lang = m[1];
    let depth = 1;
    let i = m.index + m[0].length;
    // Walk the brace nesting BUT skip over string literals so a
    // value containing `{` or `}` (rare but legal — e.g. an
    // error-format-string template that quotes braces) doesn't
    // throw the depth tracker off and prematurely close the block.
    let inString = null; // " | ' | null
    while (i < src.length) {
      const ch = src[i];
      if (inString) {
        if (ch === "\\") {
          i += 2;
          continue;
        }
        if (ch === inString) {
          inString = null;
        }
        i++;
        continue;
      }
      if (ch === '"' || ch === "'") {
        inString = ch;
        i++;
        continue;
      }
      if (ch === "{") depth++;
      else if (ch === "}") {
        depth--;
        if (depth === 0) break;
      }
      i++;
    }
    if (depth !== 0) continue;
    const block = src.slice(m.index, i);
    const keyRe = /^\s+([a-zA-Z0-9_]+)\s*:\s*"/gm;
    const keys = new Set();
    let k;
    while ((k = keyRe.exec(block)) !== null) keys.add(k[1]);
    out[lang] = { keys, end: i };
  }
  return out;
}

function escapeTs(s) {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"').replace(/\n/g, "\\n");
}

let src = fs.readFileSync(I18N_PATH, "utf8");
const tsFiles = walkSrc(SRC_ROOT);
const phantomMap = new Map();
for (const f of tsFiles) {
  for (const [k, v] of extractPhantoms(fs.readFileSync(f, "utf8"))) {
    if (!phantomMap.has(k)) phantomMap.set(k, v);
  }
}

const en = loadEnBlock(src);
const enMissing = [...phantomMap.entries()].filter(([k]) => !en.keys.has(k));

process.stdout.write(
  `[i18n-fill] phantom (used in code, missing from en): ${enMissing.length}\n`,
);

if (enMissing.length > 0) {
  const insertion = enMissing
    .map(([k, v]) => `    ${k}: "${escapeTs(v)}",`)
    .join("\n");
  const before = src.slice(0, en.blockEnd);
  const after = src.slice(en.blockEnd);
  const trimmed = before.replace(/\n+$/, "");
  if (!dryRun) {
    src = `${trimmed}\n${insertion}\n  ${after}`;
    fs.writeFileSync(I18N_PATH, src, "utf8");
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

// ── Phase 2: translate en → other langs via MyMemory ───────────

src = fs.readFileSync(I18N_PATH, "utf8");
const enFresh = loadEnBlock(src);
const langBlocks = loadLangBlocks(src);

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

const TARGET_LANGS = Object.keys(langBlocks).filter((l) => l !== "en");

async function fillLang(lang) {
  const blockInfo = langBlocks[lang];
  if (!blockInfo) return { added: 0, failed: 0 };
  const missing = [];
  for (const [k] of enFresh.values) {
    if (!blockInfo.keys.has(k)) missing.push(k);
  }
  if (missing.length === 0) return { added: 0, failed: 0 };
  process.stdout.write(`[i18n-fill] ${lang}: ${missing.length} missing\n`);
  const additions = [];
  let added = 0;
  let failed = 0;
  let quotaHit = false;
  for (const k of missing) {
    const enValue = enFresh.values.get(k) ?? k;
    try {
      const translated = await translateOne(enValue, lang);
      additions.push({ key: k, value: translated });
      added++;
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
  if (additions.length > 0 && !dryRun) {
    const cur = fs.readFileSync(I18N_PATH, "utf8");
    const blocks = loadLangBlocks(cur);
    const info = blocks[lang];
    if (info) {
      const insertion = additions
        .map((a) => `    ${a.key}: "${escapeTs(a.value)}",`)
        .join("\n");
      const before = cur.slice(0, info.end);
      const after = cur.slice(info.end);
      const trimmed = before.replace(/\n+$/, "");
      fs.writeFileSync(I18N_PATH, `${trimmed}\n${insertion}\n  ${after}`, "utf8");
    }
  }
  return { added, failed, quotaHit };
}

(async () => {
  let totalAdded = 0;
  let totalFailed = 0;
  for (const lang of TARGET_LANGS) {
    const r = await fillLang(lang);
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
  // Refresh the allowlist; gate now reflects whatever's left.
  if (!dryRun) {
    spawnSync(
      "node",
      [path.join(repoRoot, "scripts/i18n-coverage.mjs"), "--update-allowlist"],
      { stdio: "inherit" },
    );
  }
})();
