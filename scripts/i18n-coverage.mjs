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
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const ALLOWLIST_PATH = path.join(repoRoot, "scripts/i18n-known-missing.json");

const args = process.argv.slice(2);
const updateAllowlist = args.includes("--update-allowlist");
const report = args.includes("--report");

/**
 * Load translations from the per-locale split layout under
 * `client/src/i18n/locales/<code>.ts`.
 *
 * Each locale file is plain TS with one runtime surface: a top-level
 * `const <safeId>: Translations = { ... }` object literal followed by
 * `export default <safeId>;`. We extract that literal as a string and
 * evaluate it in an isolated V8 context (`vm.runInNewContext`) so the
 * `import type` line + `export default` don't trip up the parser.
 *
 * The locale ID inside the file uses underscores (e.g. `zh_CN`) to
 * stay a valid JS identifier; the filename keeps the canonical
 * hyphenated form (`zh-CN.ts`). Both this script and the runtime
 * code rely on the filename — the in-file identifier is internal.
 *
 * Why vm.runInNewContext: runs in a fresh global with no access to
 * require/process/fs/etc — the strongest sandbox Node ships without
 * spawning a subprocess.
 *
 * Why not `import(...)` directly: Node can't load .ts files without
 * a TS loader (tsx, ts-node) which we'd need to install as a script-
 * only dep. Extracting the literal inline keeps the dep graph
 * minimal.
 *
 * Replaces the older monolithic `client/src/i18n.ts` scanner
 * (extracted a single `const translations = { ... }` block). The
 * locale-split refactor moved each locale to its own file so Vite
 * could chunk-split the i18n bundle by ~50 KB lazy-load groups.
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
    const src = fs.readFileSync(fp, "utf8");
    // Find the object literal: `const <id>: Translations = { ... };`
    // The id might contain underscores for hyphenated locales
    // (zh-CN → zh_CN inside the file). Match on any identifier.
    const startMatch = src.match(
      /^const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*/m,
    );
    if (!startMatch) {
      throw new Error(
        `${fp}: could not find 'const <name>: Translations = ' declaration`,
      );
    }
    const startIdx = startMatch.index + startMatch[0].length;
    // The literal ends at `};` followed by the export default line.
    // Look for `};\n\nexport default` (matches our generator's output).
    const endMatch = src.slice(startIdx).match(/\n};\s*\n\s*export default/);
    if (!endMatch) {
      throw new Error(`${fp}: could not find end of literal (};\\nexport default)`);
    }
    const literalSrc = src.slice(
      startIdx,
      startIdx + endMatch.index + "\n}".length,
    );
    // Duplicate-key guard. A repeated key in a locale literal is valid JS
    // (the later one silently wins), so the vm eval below — and the coverage
    // diff that follows — can't see it: it just quietly overrides a
    // translation. Scan the raw literal source (one key per line in the
    // generated files) for repeats before evaluating.
    {
      const keyRe = /^\s*"?([A-Za-z][A-Za-z0-9_.-]*)"?\s*:/gm;
      const seen = new Set();
      const dups = new Set();
      let km;
      while ((km = keyRe.exec(literalSrc))) {
        if (seen.has(km[1])) dups.add(km[1]);
        seen.add(km[1]);
      }
      if (dups.size > 0) {
        throw new Error(
          `${fp}: duplicate locale key(s) — a dup silently overrides a ` +
            `translation: ${[...dups].sort().join(", ")}`,
        );
      }
    }
    const dict = vm.runInNewContext(`(${literalSrc})`, Object.create(null), {
      timeout: 1000,
    });
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

/**
 * Scan `client/src` for i18n keys referenced via `tr("literal", …)` (and
 * `t(lang, "literal", …)`) and return the set of those NOT present in en.ts.
 *
 * Why this matters: the per-language coverage check above only validates
 * translations OF en.ts keys. A key used at a call site with an inline
 * fallback — `tr("foo", undefined, "Foo")` — but never added to en.ts is
 * INVISIBLE to that check, yet it renders the English fallback for every
 * user (including non-English locales) and can never be translated. This
 * catches that class (fixed a batch of 70 such keys in the PR that added
 * this guard). Multi-line `tr(\n  "key"…)` calls are matched too.
 *
 * Only LITERAL first-arg keys are checked; dynamically-built keys
 * (`tr(\`x.${k}\`)`) can't be statically resolved and are skipped — same
 * limitation find-orphan-i18n.mjs documents. `foo`/`x` are the doc-comment
 * example keys in i18n.ts / lang.ts and are ignored.
 */
function findUsedButMissing(enKeys) {
  const SRC_DIR = path.join(repoRoot, "client/src");
  const IGNORE = new Set(["foo", "x"]);
  const used = new Set();
  // tr( <ws/nl> "key"   — and   t( <ws> ident/"lang", <ws/nl> "key"
  const trPat = /\btr\(\s*"([a-zA-Z0-9_.]+)"/g;
  const walk = (dir) => {
    for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
      const fp = path.join(dir, ent.name);
      if (ent.isDirectory()) {
        if (ent.name === "i18n" && dir.endsWith("/src")) continue; // skip locale defs
        walk(fp);
      } else if (/\.tsx?$/.test(ent.name)) {
        const txt = fs.readFileSync(fp, "utf8");
        let m;
        while ((m = trPat.exec(txt)) !== null) used.add(m[1]);
      }
    }
  };
  walk(SRC_DIR);
  return [...used]
    .filter((k) => !IGNORE.has(k) && !enKeys.has(k))
    .sort();
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
