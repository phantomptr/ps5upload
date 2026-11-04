#!/usr/bin/env node
/*
 * Keys in client/src/i18n/locales/en.ts that nothing under client/src can reach.
 * Translators are asked to translate these for nothing, so validate-repo.mjs keeps the count at the
 * budget below. What counts as used is in scripts/i18n-usage.mjs.
 *
 *   node scripts/find-orphan-i18n.mjs            list them
 *   node scripts/find-orphan-i18n.mjs --max 0    also exit 1 when there are more than 0
 *
 * To delete them from all 21 locale files: node scripts/i18n-prune-unused.mjs --apply
 */
import path from "node:path";
import { fileURLToPath } from "node:url";
import { readLocale } from "./i18n-locale-file.mjs";
import { classifyKeys, scanClientSource } from "./i18n-usage.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const maxAt = args.indexOf("--max");
const max = maxAt >= 0 ? Number(args[maxAt + 1]) : null;
if (maxAt >= 0 && !Number.isInteger(max)) {
  console.error("find-orphan-i18n: --max needs a number");
  process.exit(2);
}

const enKeys = Object.keys(readLocale(path.join(repoRoot, "client/src/i18n/locales/en.ts")));
const { dead, dynamic } = classifyKeys(enKeys, scanClientSource(repoRoot));

console.log(`[find-orphan-i18n] ${enKeys.length} keys, ${dead.length} orphans, ${dynamic.length} reached only by a built key`);
for (const k of dead) console.log(`  ${k}`);
if (max !== null && dead.length > max) {
  console.error(`[find-orphan-i18n] ${dead.length} orphan keys, the budget is ${max}. Use them, or delete them: node scripts/i18n-prune-unused.mjs --apply`);
  process.exit(1);
}
