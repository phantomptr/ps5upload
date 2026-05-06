#!/usr/bin/env node
/*
 * Diagnostic: list i18n keys defined in `client/src/i18n.ts` (English
 * is the canonical superset) that are not referenced by any source
 * file under `client/src/`.
 *
 * Advisory only — keys accessed via template-string interpolation
 * (`tr(\`status.${kind}\`)`) won't appear as literal strings and
 * are flagged as "dynamic" rather than "orphan." Manual review is
 * required before deleting; the script just narrows the list.
 *
 * Run: `node scripts/find-orphan-i18n.mjs`
 */
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const I18N_PATH = path.join(repoRoot, "client/src/i18n.ts");

function loadEnKeys() {
  const src = fs.readFileSync(I18N_PATH, "utf8");
  const startIdx = src.search(/^const translations\s*=\s*/m);
  const cutoff = src.indexOf("export type LanguageCode");
  if (startIdx < 0 || cutoff < 0) {
    throw new Error("i18n.ts: could not find translations literal");
  }
  const literal = src
    .slice(startIdx, cutoff)
    .replace(/^const translations\s*=\s*/, "")
    .trim()
    .replace(/;\s*$/, "");
  const obj = vm.runInNewContext(`(${literal})`, Object.create(null), {
    timeout: 1000,
  });
  return Object.keys(obj.en);
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
    .filter((f) => /\.(tsx?|ts)$/.test(f) && !f.endsWith("i18n.ts"));
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
