#!/usr/bin/env node
/*
 * The console web page's translations: every `cw_*` key from all 21 locale
 * files under client/src/i18n/locales/, written one file per locale to
 * client/src/console/locales/<locale>.json as { "cw_…": "…" }.
 *
 * The page (client/src/console/strings.ts) embeds only en.json; the build
 * (vite.console.config.ts, consoleStrings) emits every other locale as
 * dist-console/strings/<locale>.json, which payload/Makefile gzips into the
 * helper and the page fetches from GET /strings/<locale>.json. `npm run
 * build:console` runs this first; the files are committed so typecheck and
 * tests work without a build, and strings.test.ts fails when they are stale.
 *
 * Locale files are read as the `const <id>: Translations = { … }` literal
 * evaluated in an isolated vm context, never regex-parsed for values.
 * scripts/i18n-locale-file.mjs is the other reader: keep them in step if the
 * locale-file shape changes.
 *
 * Usage: node scripts/build-console-strings.mjs [--check]
 *   --check  exit 1 when the committed file differs, write nothing.
 */
import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES_DIR = path.join(repoRoot, "client/src/i18n/locales");
const OUT_DIR = path.join(repoRoot, "client/src/console/locales");
const PREFIX = "cw_";

function loadLocale(fp) {
  const src = fs.readFileSync(fp, "utf8");
  const startMatch = src.match(/^const\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*Translations\s*=\s*/m);
  if (!startMatch) throw new Error(`${fp}: could not find 'const <name>: Translations = ' declaration`);
  const startIdx = startMatch.index + startMatch[0].length;
  const endMatch = src.slice(startIdx).match(/\n};\s*\n\s*export default/);
  if (!endMatch) throw new Error(`${fp}: could not find end of literal (};\\nexport default)`);
  const literalSrc = src.slice(startIdx, startIdx + endMatch.index + "\n}".length);
  return vm.runInNewContext(`(${literalSrc})`, Object.create(null), { timeout: 1000 });
}

/** { locale: { cw_key: value } }, locales and keys sorted, English required. */
export function extractConsoleStrings() {
  const files = fs
    .readdirSync(LOCALES_DIR)
    .filter((f) => f.endsWith(".ts") && !f.endsWith(".d.ts"))
    .sort();
  const out = {};
  for (const f of files) {
    const dict = loadLocale(path.join(LOCALES_DIR, f));
    const picked = {};
    for (const key of Object.keys(dict).sort()) {
      if (key.startsWith(PREFIX)) picked[key] = dict[key];
    }
    out[f.replace(/\.ts$/, "")] = picked;
  }
  if (!out.en) throw new Error(`${LOCALES_DIR} has no en.ts`);
  return out;
}

/** { "<locale>.json": file text } for every locale, as committed under OUT_DIR. */
export function consoleStringFiles() {
  const out = {};
  for (const [code, map] of Object.entries(extractConsoleStrings())) {
    out[`${code}.json`] = `${JSON.stringify(map, null, 2)}\n`;
  }
  return out;
}

function main() {
  const want = consoleStringFiles();
  const have = fs.existsSync(OUT_DIR) ? fs.readdirSync(OUT_DIR).filter((f) => f.endsWith(".json")) : [];
  const stale = Object.keys(want).filter((f) => !have.includes(f) || fs.readFileSync(path.join(OUT_DIR, f), "utf8") !== want[f]);
  const extra = have.filter((f) => !(f in want));
  if (process.argv.includes("--check")) {
    if (stale.length || extra.length) {
      console.error(`${path.relative(repoRoot, OUT_DIR)} is stale (${[...stale, ...extra].join(", ")}): run node scripts/build-console-strings.mjs`);
      process.exit(1);
    }
    return;
  }
  // Rewrite only what changed, so the payload's make rule does not see a new mtime for nothing.
  fs.mkdirSync(OUT_DIR, { recursive: true });
  for (const f of stale) fs.writeFileSync(path.join(OUT_DIR, f), want[f]);
  for (const f of extra) fs.unlinkSync(path.join(OUT_DIR, f));
  const n = Object.keys(JSON.parse(want["en.json"])).length;
  console.log(`console strings: ${n} keys × ${Object.keys(want).length} locales`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
