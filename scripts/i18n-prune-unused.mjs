#!/usr/bin/env node
/*
 * i18n dead-key pruner.
 *
 * Finds the keys of client/src/i18n/locales/en.ts that nothing under client/src can reach (see
 * scripts/i18n-usage.mjs) and, with --apply, deletes them from every locale file under
 * client/src/i18n/locales (English and all the translations), from the allowlist
 * scripts/i18n-known-missing.json and from the source hashes scripts/i18n-source-hashes.json.
 * Only the lines of the deleted entries change. If a `cw_*` key goes, the console page's generated
 * locales are rebuilt too.
 *
 * `tr(IDENTIFIER, …)` sites are listed as a warning, not a refusal: the scan already counts any
 * quoted key and any template that can build one, so those sites stay covered unless the key is
 * glued together some other way. Read the dead list before --apply.
 *
 * Modes:
 *   default          List dead keys and the tr(IDENTIFIER) sites.
 *   --apply          Delete the dead keys.
 */

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { localeCodes, readLocale, removeKeys } from "./i18n-locale-file.mjs";
import { dropKeys } from "./i18n-source-hashes.mjs";
import { classifyKeys, scanClientSource, withoutKeys } from "./i18n-usage.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const ALLOWLIST_PATH = path.join(repoRoot, "scripts/i18n-known-missing.json");
const SRC_ROOT = path.join(repoRoot, "client/src");

const apply = process.argv.includes("--apply");

/** `tr(x)` / `tr(a.b, …)`: the key is a runtime value. Listed so a human can look. */
function identifierCalls(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules" || entry.name.startsWith(".")) continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (full === LOCALES_DIR) continue;
      identifierCalls(full, out);
    } else if (/\.tsx?$/.test(entry.name) && !/\.test\./.test(entry.name)) {
      const content = fs.readFileSync(full, "utf8");
      for (const m of content.matchAll(/\btr\s*\(\s*([A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*)\s*[,?)]/g)) {
        const line = content.slice(0, m.index).split("\n").length;
        out.push(`${path.relative(repoRoot, full)}:${line} → ${m[0].slice(0, 60).trim()}`);
      }
    }
  }
  return out;
}

const enKeys = Object.keys(readLocale(path.join(LOCALES_DIR, "en.ts")));
const { dead, dynamic } = classifyKeys(enKeys, scanClientSource(repoRoot));
const sites = identifierCalls(SRC_ROOT);

process.stdout.write(`[i18n-prune] en=${enKeys.length} dead=${dead.length} built=${dynamic.length} tr(IDENTIFIER) sites=${sites.length}\n`);
if (sites.length > 0) {
  process.stdout.write("[i18n-prune] warning: these sites take their key at run time; a key only they can reach through unusual string building would look dead:\n");
  for (const site of sites) process.stdout.write(`  ${site}\n`);
}

if (!apply) {
  process.stdout.write("[i18n-prune] dead keys:\n");
  for (const k of dead) process.stdout.write(`  ${k}\n`);
  process.stdout.write("\n[i18n-prune] re-run with --apply to delete them from every locale.\n");
  process.exit(0);
}

if (dead.length === 0) {
  process.stdout.write("[i18n-prune] nothing to prune.\n");
  process.exit(0);
}

let stripped = 0;
for (const code of localeCodes(LOCALES_DIR, ".ts")) {
  const fp = path.join(LOCALES_DIR, `${code}.ts`);
  const orig = fs.readFileSync(fp, "utf8");
  const present = dead.filter((k) => k in readLocale(fp));
  stripped += present.length;
  const next = removeKeys(orig, present);
  if (next !== orig) fs.writeFileSync(fp, next, "utf8");
}

// The allowlist: a deleted key cannot be missing, stale or untranslated any more.
let allowlistShrunk = 0;
if (fs.existsSync(ALLOWLIST_PATH)) {
  const { allowlist, removed } = withoutKeys(JSON.parse(fs.readFileSync(ALLOWLIST_PATH, "utf8")), dead);
  allowlistShrunk = removed;
  fs.writeFileSync(ALLOWLIST_PATH, `${JSON.stringify(allowlist, null, 2)}\n`, "utf8");
}

dropKeys(repoRoot, "app", dead);

if (dead.some((k) => k.startsWith("cw_"))) {
  const res = spawnSync(process.execPath, [path.join(repoRoot, "scripts/build-console-strings.mjs")], { stdio: "inherit" });
  if (res.status !== 0) process.exit(res.status ?? 1);
}

process.stdout.write(`[i18n-prune] deleted ${stripped} entries across the locale files and ${allowlistShrunk} allowlist entries.\n`);
