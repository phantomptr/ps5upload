#!/usr/bin/env node
/*
 * Diagnostic: list i18n keys defined in `client/src/i18n/locales/en.ts`
 * (English is the canonical superset) that are not referenced by any
 * source file under `client/src/`.
 *
 * Advisory only — keys accessed via template-string interpolation
 * (`tr(\`status.${kind}\`)`) won't appear as literal strings and
 * are flagged as "dynamic" rather than "orphan." Manual review is
 * required before deleting; the script just narrows the list.
 *
 * Run: `node scripts/find-orphan-i18n.mjs`
 *
 * Locale-split aware: the old monolithic `client/src/i18n.ts` (one
 * `const translations = { en: {...}, ... }` block) is gone — each
 * locale now lives in `client/src/i18n/locales/<code>.ts`. We read
 * the canonical `en.ts` literal the same isolated-vm way
 * `i18n-coverage.mjs` does.
 */
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const EN_PATH = path.join(LOCALES_DIR, "en.ts");

/** Extract the `const <id>: Translations = { ... }` literal from a
 *  locale file and evaluate it in an isolated V8 context. Mirrors
 *  `loadTranslations()` in scripts/i18n-coverage.mjs — keep the two in
 *  sync if the locale-file shape ever changes. */
function loadLocaleDict(fp) {
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
  const literalSrc = src.slice(startIdx, startIdx + endMatch.index + "\n}".length);
  return vm.runInNewContext(`(${literalSrc})`, Object.create(null), { timeout: 1000 });
}

function loadEnKeys() {
  if (!fs.existsSync(EN_PATH)) {
    throw new Error(`canonical locale not found: ${EN_PATH}`);
  }
  return Object.keys(loadLocaleDict(EN_PATH));
}

function readAllClientSrc() {
  // Use spawnSync without a shell — argv goes straight to git.
  const res = spawnSync("git", ["ls-files", "client/src"], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (res.status !== 0) {
    throw new Error(res.stderr || "git ls-files failed");
  }
  const files = res.stdout
    .trim()
    .split("\n")
    // Exclude the locale dictionaries themselves — they *define* the
    // keys, they don't *reference* them. Including them would make
    // every key look "used".
    .filter(
      (f) =>
        /\.(tsx?|ts)$/.test(f) &&
        !f.startsWith("client/src/i18n/locales/") &&
        f !== "client/src/i18n.ts",
    );
  return files
    .map((f) => fs.readFileSync(path.join(repoRoot, f), "utf8"))
    .join("\n");
}

const enKeys = loadEnKeys();
const allSrc = readAllClientSrc();

const orphans = [];
const dynamic = [];
for (const key of enKeys) {
  // Literal match — any source contains the key inside "..." / '...' / `...`.
  const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const literalRe = new RegExp(`["'\`]${escaped}["'\`]`);
  if (literalRe.test(allSrc)) continue;

  // Else: walk back through key prefixes looking for a template-
  // literal expression that could reach this key. Two shapes are
  // common in this codebase:
  //   - dotted:    `prefix.${var}`        (e.g. `status.${kind}`)
  //   - underscored: `prefix_${var}`      (e.g. `log_level_${level}`)
  // We split on every separator boundary (.) AND every char to also
  // catch pure-prefix interpolation (`log_level_${l}` covers
  // log_level_debug / _info / _warn / _error).
  const parts = key.split(/[._]/);
  let isDynamic = false;
  for (let i = parts.length - 1; i >= 1; i--) {
    // Reconstruct the prefix using whichever separator the key uses
    // at this boundary. We try both `.` and `_`.
    for (const sep of [".", "_"]) {
      const prefix = parts.slice(0, i).join(sep);
      const prefixEsc = prefix.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      // Match `` `<prefix><sep>${ `` — the same `\`status.${kind}\``
      // and `\`log_level_${level}\`` shapes both routed here.
      const dynamicRe = new RegExp("`" + prefixEsc + sep.replace(".", "\\.") + "\\$\\{");
      if (dynamicRe.test(allSrc)) {
        isDynamic = true;
        break;
      }
    }
    if (isDynamic) break;
  }
  if (isDynamic) {
    dynamic.push(key);
  } else {
    orphans.push(key);
  }
}

console.log("[find-orphan-i18n]", {
  totalEnKeys: enKeys.length,
  orphanCount: orphans.length,
  dynamicCount: dynamic.length,
});
console.log(`\n=== Likely orphan (${orphans.length}) ===`);
orphans.sort().forEach((k) => console.log(`  ${k}`));
console.log(`\n=== Dynamic-referenced (${dynamic.length}) ===`);
dynamic.sort().forEach((k) => console.log(`  ${k}`));
