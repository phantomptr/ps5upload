#!/usr/bin/env node
/*
 * Applies translations that branches wrote to sidecar files, so 21 locale files don't conflict on
 * every merge. Each *.json in the directory has the shape
 *
 *   { "app": { key: { lang: text } }, "console": { … }, "site": { … } }
 *
 *   app      client/src/i18n/locales/<lang>.ts
 *   console  the Minion page's cw_* strings: client/src/console/locales/<lang>.json
 *   site     site/i18n/<lang>.json
 *
 * client/src/console/locales is generated from the cw_* keys of the app's locale files
 * (scripts/build-console-strings.mjs), so a cw_* key goes to both places whichever section names
 * it. Written only to the JSON, the next build would drop it. The same goes for English that a branch
 * changed or added only in the console's en.json: it moves to en.ts, unless en.ts changed too (then
 * en.ts wins).
 *
 *   node scripts/i18n-apply-pending.mjs <dir>                    apply
 *   node scripts/i18n-apply-pending.mjs <dir> --check            report only, write nothing
 *   node scripts/i18n-apply-pending.mjs <dir> --replace-drifted  also replace a translation whose
 *                                                                English changed since it was
 *                                                                recorded (i18n-source-hashes.mjs)
 *
 * Each key lands beside its English neighbours, on a line of its own; nothing else in the file
 * moves. Refused, with nothing written at all: a placeholder or tag mismatch, a key English doesn't
 * have, a language with no locale file, two sidecars disagreeing, and a key already present with a
 * different value. Applying the same files again changes nothing.
 *
 * Afterwards record the result: npm run i18n:hashes
 */
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { insertEntries, readLocale, setValues } from "./i18n-locale-file.mjs";
import { checkSourceHashes, englishMovedSinceRecorded } from "./i18n-source-hashes.mjs";

const APP_DIR = "client/src/i18n/locales";
const CONSOLE_DIR = "client/src/console/locales";
const SITE_DIR = "site/i18n";
const SECTIONS = ["app", "console", "site"];

/** Placeholders, and for the website the tags too (scripts/site-i18n.mjs asks the same). */
const shapeOf = (section, text) => {
  const re = section === "site" ? /<[^>]+>|\{\w+\}/g : /\{[A-Za-z_][A-Za-z0-9_]*\}/g;
  return (text.match(re) ?? []).sort().join(" ");
};

function readSidecars(dir) {
  const refused = [];
  const wanted = { app: {}, console: {}, site: {} };
  const names = fs.readdirSync(dir).filter((f) => f.endsWith(".json")).sort();
  for (const name of names) {
    let doc;
    try {
      doc = JSON.parse(fs.readFileSync(path.join(dir, name), "utf8"));
    } catch (e) {
      refused.push(`${name}: not valid JSON: ${e.message}`);
      continue;
    }
    for (const section of Object.keys(doc)) {
      if (!SECTIONS.includes(section)) {
        refused.push(`${name}: unknown section "${section}"`);
        continue;
      }
      for (const [key, byLang] of Object.entries(doc[section] ?? {})) {
        for (const [lang, text] of Object.entries(byLang)) {
          const slot = (wanted[section][key] ??= {});
          const before = slot[lang];
          if (typeof text !== "string" || !text.trim()) {
            refused.push(`${name}: ${section} ${key} [${lang}]: not a text`);
          } else if (before && before.text !== text) {
            refused.push(`${section} ${key} [${lang}]: ${before.from} and ${name} give different texts`);
          } else {
            slot[lang] = { text, from: before?.from ?? name };
          }
        }
      }
    }
  }
  return { wanted, refused, files: names.length };
}

/** The nearest English neighbour already in the file: the one before, else the one after. */
function placeAfter(key, enOrder, present) {
  const at = enOrder.indexOf(key);
  if (at < 0) return { after: null };
  for (let i = at - 1; i >= 0; i--) if (present.has(enOrder[i])) return { after: enOrder[i] };
  return { after: null };
}

/**
 * The plan for one file: new entries and replacements. `order` is the key order new entries follow.
 * Keys present with another value go to `conflicts`.
 */
function planFile(file, items, order, { json, replaceable }) {
  const dict = readLocale(file);
  const present = new Set(Object.keys(dict));
  const inserts = [];
  const replaces = {};
  const conflicts = [];
  const byOrder = [...items].sort((a, b) => order.indexOf(a.key) - order.indexOf(b.key));
  for (const { key, text } of byOrder) {
    if (key in dict) {
      if (dict[key] === text) continue;
      if (replaceable.has(key)) replaces[key] = text;
      else conflicts.push({ key, have: dict[key] });
      continue;
    }
    inserts.push({ key, value: text, ...placeAfter(key, order, present) });
    present.add(key);
  }
  return { file, json, inserts, replaces, conflicts };
}

/** The generated JSON is sorted by key; a new key goes where the next build would put it. */
function sortedOrder(file, extra) {
  return [...new Set([...Object.keys(readLocale(file)), ...extra])].sort();
}

/**
 * cw_* English that only the console's en.json has. The next build of that file starts from en.ts and
 * would drop it. Left alone when en.ts changed since the hashes were recorded: there en.json is merely
 * behind.
 */
function consoleOnlyEnglish(root, enApp, consoleEn) {
  const out = {};
  for (const [key, text] of Object.entries(consoleEn)) {
    if (!key.startsWith("cw_") || enApp[key] === text) continue;
    if (key in enApp && englishMovedSinceRecorded(root, "app", key, enApp[key])) continue;
    out[key] = text;
  }
  return out;
}

/** The en.ts edits that take over `promoted`, and en.ts's key order with the new keys in their place. */
function promoteToEnglish(enApp, promoted) {
  const order = Object.keys(enApp);
  const present = new Set(order);
  const inserts = [];
  const replaces = {};
  const sorted = [...new Set([...order.filter((k) => k.startsWith("cw_")), ...Object.keys(promoted)])].sort();
  for (const key of sorted) {
    if (!(key in promoted)) continue;
    if (present.has(key)) {
      replaces[key] = promoted[key];
      continue;
    }
    const { after } = placeAfter(key, sorted, present);
    inserts.push({ key, value: promoted[key], after: after ?? order.at(-1) ?? null });
    order.splice(after === null ? order.length : order.indexOf(after) + 1, 0, key);
    present.add(key);
  }
  return { inserts, replaces, order };
}

export function applyPending({ root, dir, check = false, replaceDrifted = false }) {
  const { wanted, refused, files } = readSidecars(dir);
  const enAppOnDisk = readLocale(path.join(root, APP_DIR, "en.ts"));
  const consoleEnFile = path.join(root, CONSOLE_DIR, "en.json");
  const consoleEn = fs.existsSync(consoleEnFile) ? readLocale(consoleEnFile) : {};
  const promoted = consoleOnlyEnglish(root, enAppOnDisk, consoleEn);
  const promotion = promoteToEnglish(enAppOnDisk, promoted);
  const enApp = { ...enAppOnDisk, ...promoted };
  const enSiteFile = path.join(root, SITE_DIR, "en.json");
  const enSite = fs.existsSync(enSiteFile) ? readLocale(enSiteFile) : {};
  const appOrder = promotion.order;
  const siteOrder = Object.keys(enSite);

  const drifted = { app: new Map(), site: new Map() };
  if (replaceDrifted) {
    for (const d of checkSourceHashes(root).drift) drifted[d.set].set(d.key, new Set(d.langs));
  }
  const promotedKeys = new Set(Object.keys(promotion.replaces));

  // Merge into per-language lists: app and console share the .ts files.
  const perLang = { ts: {}, consoleJson: {}, siteJson: {} };
  const add = (bucket, lang, item) => (bucket[lang] ??= []).push(item);
  for (const section of SECTIONS) {
    for (const [key, byLang] of Object.entries(wanted[section])) {
      const english = section === "site" ? enSite[key] : enApp[key];
      if (english === undefined) {
        refused.push(`${section} ${key}: not in the English catalogue (${section === "site" ? `${SITE_DIR}/en.json` : `${APP_DIR}/en.ts`})`);
        continue;
      }
      if (section === "console" && !key.startsWith("cw_")) {
        refused.push(`console ${key}: the Minion page only takes cw_* keys`);
        continue;
      }
      for (const [lang, { text, from }] of Object.entries(byLang)) {
        if (lang === "en") {
          refused.push(`${section} ${key}: ${from} has text for en; English lives in the source catalogue`);
          continue;
        }
        const target = section === "site" ? `${SITE_DIR}/${lang}.json` : `${APP_DIR}/${lang}.ts`;
        if (!fs.existsSync(path.join(root, target))) {
          refused.push(`${section} ${key} [${lang}]: no ${target}`);
          continue;
        }
        if (shapeOf(section, text) !== shapeOf(section, english)) {
          refused.push(`${section} ${key} [${lang}]: placeholders differ: en=[${shapeOf(section, english)}] ${lang}=[${shapeOf(section, text)}]`);
          continue;
        }
        const item = { key, text };
        if (section === "site") {
          add(perLang.siteJson, lang, item);
        } else {
          if (!perLang.ts[lang]?.some((i) => i.key === key)) add(perLang.ts, lang, item);
          else if (perLang.ts[lang].find((i) => i.key === key).text !== text) {
            refused.push(`${key} [${lang}]: the app and console sections give different texts`);
          }
          if (key.startsWith("cw_") && !perLang.consoleJson[lang]?.some((i) => i.key === key)) add(perLang.consoleJson, lang, item);
        }
      }
    }
  }

  const plans = [];
  const replaceableFor = (set, lang) => {
    const out = new Set();
    for (const [key, langs] of drifted[set]) if (langs.has(lang)) out.add(key);
    if (set === "app" && replaceDrifted) for (const key of promotedKeys) out.add(key);
    return out;
  };
  for (const [lang, items] of Object.entries(perLang.ts)) {
    plans.push(planFile(path.join(root, APP_DIR, `${lang}.ts`), items, appOrder, { json: false, replaceable: replaceableFor("app", lang) }));
  }
  for (const [lang, items] of Object.entries(perLang.consoleJson)) {
    const file = path.join(root, CONSOLE_DIR, `${lang}.json`);
    if (!fs.existsSync(file)) continue; // It is a build product: the next build writes it.
    const order = sortedOrder(file, items.map((i) => i.key));
    plans.push(planFile(file, items, order, { json: true, replaceable: replaceableFor("app", lang) }));
  }
  for (const [lang, items] of Object.entries(perLang.siteJson)) {
    plans.push(planFile(path.join(root, SITE_DIR, `${lang}.json`), items, siteOrder, { json: true, replaceable: replaceableFor("site", lang) }));
  }

  for (const plan of plans) {
    const rel = path.relative(root, plan.file);
    for (const c of plan.conflicts) refused.push(`${rel}: ${c.key} is already there with a different value: ${c.have}`);
  }

  // The console page's English: a branch that only touched en.ts leaves en.json behind.
  const consoleEnItems = [];
  if (fs.existsSync(consoleEnFile)) {
    const keys = new Set([...Object.values(perLang.consoleJson).flat().map((i) => i.key)]);
    for (const key of keys) if (consoleEn[key] !== enApp[key]) consoleEnItems.push({ key, text: enApp[key] });
  }

  const summary = {
    files,
    inserted: plans.reduce((n, p) => n + p.inserts.length, 0),
    replaced: plans.reduce((n, p) => n + Object.keys(p.replaces).length, 0),
    refused,
    changedFiles: [],
  };
  if (refused.length > 0 || check) return summary;

  if (consoleEnItems.length > 0) {
    const order = sortedOrder(consoleEnFile, consoleEnItems.map((i) => i.key));
    plans.push(planFile(consoleEnFile, consoleEnItems, order, { json: true, replaceable: new Set(consoleEnItems.map((i) => i.key)) }));
  }
  if (Object.keys(promoted).length > 0) {
    plans.push({ file: path.join(root, APP_DIR, "en.ts"), json: false, inserts: promotion.inserts, replaces: promotion.replaces, conflicts: [] });
  }
  for (const plan of plans) {
    const src = fs.readFileSync(plan.file, "utf8");
    let next = src;
    if (Object.keys(plan.replaces).length > 0) next = setValues(next, plan.replaces, { json: plan.json });
    if (plan.inserts.length > 0) next = insertEntries(next, plan.inserts, { json: plan.json });
    if (next !== src) {
      fs.writeFileSync(plan.file, next);
      summary.changedFiles.push(path.relative(root, plan.file));
    }
  }
  return summary;
}

function main() {
  const args = process.argv.slice(2);
  const dir = args.find((a) => !a.startsWith("--"));
  if (!dir || !fs.existsSync(dir) || !fs.statSync(dir).isDirectory()) {
    console.error("usage: node scripts/i18n-apply-pending.mjs <dir> [--check] [--replace-drifted]");
    process.exit(2);
  }
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const check = args.includes("--check");
  const result = applyPending({ root, dir, check, replaceDrifted: args.includes("--replace-drifted") });
  if (result.refused.length > 0) {
    console.error(`[i18n-apply-pending] refused, nothing written (${result.refused.length}):`);
    for (const r of result.refused) console.error(`  ${r}`);
    process.exit(1);
  }
  const verb = check ? "would add" : "added";
  console.log(`[i18n-apply-pending] ${result.files} sidecar files: ${verb} ${result.inserted} entries, replaced ${result.replaced}, ${result.changedFiles.length} files changed`);
  if (!check && result.changedFiles.length > 0) console.log("  next: npm run i18n:hashes");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
