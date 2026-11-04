#!/usr/bin/env node
/*
 * Source drift: English changed, a translation did not.
 *
 * The coverage gate only sees that a key exists in every language, so a rewritten English string
 * keeps its old translation for ever: "Cancel this download" sat over "Stop watching this
 * download", the opposite of what the button does, in 20 languages for several releases.
 *
 * scripts/i18n-source-hashes.json records, per key of the app and website catalogues, a hash of the
 * English value and a short hash of each translation as they were last agreed. (The console page's
 * cw_* strings are generated from the app catalogue, so they are covered by it.) A later check
 * compares:
 *
 *   English hash differs, a translation's hash does not   drift: that language was left behind
 *   anything else that differs (new key, new translation)  not recorded yet: nothing wrong
 *
 * Both fail the gate, because a record that lags makes the next drift invisible.
 *
 *   node scripts/i18n-source-hashes.mjs               check (i18n-coverage.mjs runs this too)
 *   node scripts/i18n-source-hashes.mjs --update      record everything except drift; drift still fails
 *   node scripts/i18n-source-hashes.mjs --ack KEY...  the translations of KEY are still right as they
 *                                                     are (say, English only changed case): record it
 *
 * Normal flow when you change an English string: change it, retranslate it, run --update.
 *
 * An entry is "<8 hex of English> <3 characters per language, in the order of `langs`>". The short
 * hash is only asked "did this change?"; a collision shows as a false drift, never a hidden one.
 * The space keeps a line from looking like a credential to gitleaks when a key has "token" in it.
 */
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { localeCodes, readLocale } from "./i18n-locale-file.mjs";

const LEDGER = "scripts/i18n-source-hashes.json";
const SETS = {
  app: { dir: "client/src/i18n/locales", ext: ".ts" },
  site: { dir: "site/i18n", ext: ".json" },
};
const NO_TRANSLATION = "---";

const sha = (s) => createHash("sha256").update(s, "utf8").digest();
const hashEnglish = (s) => sha(s).subarray(0, 4).toString("hex");
const hashTranslation = (s) => (s === undefined ? NO_TRANSLATION : sha(s).toString("base64url").slice(0, 3));

function loadSet(root, name) {
  const { dir, ext } = SETS[name];
  const full = path.join(root, dir);
  const dicts = {};
  if (fs.existsSync(full)) {
    for (const code of localeCodes(full, ext)) dicts[code] = readLocale(path.join(full, `${code}${ext}`));
  }
  return dicts;
}

function loadCatalogues(root) {
  const out = {};
  for (const name of Object.keys(SETS)) {
    const dicts = loadSet(root, name);
    const { en = {}, ...translations } = dicts;
    out[name] = { en, translations };
  }
  return out;
}

function allLangs(catalogues) {
  const langs = new Set();
  for (const { translations } of Object.values(catalogues)) for (const c of Object.keys(translations)) langs.add(c);
  return [...langs].sort();
}

function readLedger(root) {
  const file = path.join(root, LEDGER);
  if (!fs.existsSync(file)) return null;
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

function writeLedger(root, ledger) {
  const sortObj = (o) => Object.fromEntries(Object.keys(o).sort().map((k) => [k, o[k]]));
  const out = { langs: ledger.langs };
  for (const name of Object.keys(SETS)) out[name] = sortObj(ledger[name] ?? {});
  fs.mkdirSync(path.join(root, path.dirname(LEDGER)), { recursive: true });
  fs.writeFileSync(path.join(root, LEDGER), `${JSON.stringify(out, null, 1)}\n`);
}

function parseEntry(entry, langs) {
  const [en, packed = ""] = entry.split(" ");
  const translations = {};
  langs.forEach((lang, i) => {
    translations[lang] = packed.slice(i * 3, i * 3 + 3);
  });
  return { en, translations };
}

function renderEntry(key, set, langs) {
  const packed = langs.map((lang) => hashTranslation(set.translations[lang]?.[key])).join("");
  return `${hashEnglish(set.en[key])} ${packed}`;
}

/**
 * { missingLedger, drift: [{ set, key, langs, english }], unrecorded: [{ set, key, why }] }.
 * A translation that now equals the English is not drift, whatever its hash says.
 */
export function checkSourceHashes(root) {
  const catalogues = loadCatalogues(root);
  const ledger = readLedger(root);
  if (!ledger) return { missingLedger: true, drift: [], unrecorded: [] };
  const langs = allLangs(catalogues);
  const drift = [];
  const unrecorded = [];
  for (const [name, set] of Object.entries(catalogues)) {
    const recorded = ledger[name] ?? {};
    for (const key of Object.keys(set.en)) {
      const entry = recorded[key];
      if (entry === undefined) {
        unrecorded.push({ set: name, key, why: "new key" });
        continue;
      }
      const was = parseEntry(entry, ledger.langs);
      const englishChanged = was.en !== hashEnglish(set.en[key]);
      const behind = [];
      let changed = englishChanged;
      for (const lang of langs) {
        const value = set.translations[lang]?.[key];
        const same = was.translations[lang] === hashTranslation(value);
        if (!same) changed = true;
        if (englishChanged && same && value !== undefined && value !== set.en[key]) behind.push(lang);
      }
      if (behind.length) drift.push({ set: name, key, langs: behind, english: set.en[key] });
      else if (changed) unrecorded.push({ set: name, key, why: englishChanged ? "English and translations changed" : "translation changed" });
    }
    for (const key of Object.keys(recorded)) {
      if (!(key in set.en)) unrecorded.push({ set: name, key, why: "key removed" });
    }
  }
  if (langs.join() !== (ledger.langs ?? []).join() && !unrecorded.length && !drift.length) {
    unrecorded.push({ set: "app", key: "(language list)", why: "languages changed" });
  }
  return { missingLedger: false, drift, unrecorded };
}

/** Record the present state of the given keys, or of everything except drift when `keys` is omitted. */
export function recordHashes(root, { acknowledge = [] } = {}) {
  const catalogues = loadCatalogues(root);
  const langs = allLangs(catalogues);
  const old = readLedger(root);
  const report = old ? checkSourceHashes(root) : { drift: [] };
  const ack = new Set(acknowledge);
  const unknown = [...ack].filter((k) => !Object.values(catalogues).some((set) => k in set.en));
  if (unknown.length) throw new Error(`not in English: ${unknown.join(", ")}`);
  const stillDrifting = new Map();
  for (const d of report.drift) if (!ack.has(d.key)) stillDrifting.set(`${d.set}\0${d.key}`, d);

  const next = { langs };
  for (const [name, set] of Object.entries(catalogues)) {
    next[name] = {};
    for (const key of Object.keys(set.en)) {
      if (stillDrifting.has(`${name}\0${key}`)) {
        // Keep the old hashes: the drift stays visible, and in the new language order.
        const was = parseEntry(old[name][key], old.langs);
        const packed = langs.map((lang) => was.translations[lang] || NO_TRANSLATION).join("");
        next[name][key] = `${was.en} ${packed}`;
      } else {
        next[name][key] = renderEntry(key, set, langs);
      }
    }
  }
  writeLedger(root, next);
  return { drift: [...stillDrifting.values()] };
}

/** Forget keys that were deleted from English, leaving every other entry as it is. */
export function dropKeys(root, setName, keys) {
  const ledger = readLedger(root);
  if (!ledger) return;
  for (const k of keys) delete ledger[setName][k];
  writeLedger(root, ledger);
}

/**
 * Has this English value changed, or appeared, since the last record? With no record at all there is
 * nothing to compare with: false.
 */
export function englishMovedSinceRecorded(root, setName, key, english) {
  const ledger = readLedger(root);
  if (!ledger) return false;
  const entry = ledger[setName]?.[key];
  return entry === undefined || parseEntry(entry, ledger.langs).en !== hashEnglish(english);
}

/** The gate's text for a failed check, or "" when all is well. */
export function describeProblems({ missingLedger, drift, unrecorded }) {
  const lines = [];
  if (missingLedger) {
    lines.push(`${LEDGER} is missing. Create it: node scripts/i18n-source-hashes.mjs --update`);
    return lines.join("\n");
  }
  if (drift.length) {
    lines.push(`English changed but the translation did not (${drift.length} keys):`);
    for (const d of drift.slice(0, 40)) {
      const who = d.langs.length > 6 ? `${d.langs.length} languages` : d.langs.join(", ");
      lines.push(`  ${d.key} (${who})`);
      lines.push(`      en: ${d.english.length > 110 ? `${d.english.slice(0, 110)}…` : d.english}`);
    }
    if (drift.length > 40) lines.push(`  … and ${drift.length - 40} more`);
    lines.push("  fix: retranslate them from the new English, then run: node scripts/i18n-source-hashes.mjs --update");
    lines.push("  still right as they are (English only changed case, say)? node scripts/i18n-source-hashes.mjs --ack KEY...");
  }
  if (unrecorded.length) {
    lines.push(`Strings changed and not recorded yet (${unrecorded.length} keys, for example ${unrecorded.slice(0, 5).map((u) => u.key).join(", ")}).`);
    lines.push("  fix: node scripts/i18n-source-hashes.mjs --update");
  }
  return lines.join("\n");
}

function main() {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
  const args = process.argv.slice(2);
  const ackAt = args.indexOf("--ack");
  if (ackAt >= 0 && args.length === ackAt + 1) {
    console.error("--ack needs at least one key");
    process.exit(2);
  }
  if (ackAt >= 0 || args.includes("--update")) {
    const acknowledge = ackAt >= 0 ? args.slice(ackAt + 1) : [];
    const { drift } = recordHashes(root, { acknowledge });
    if (drift.length) {
      console.error(describeProblems({ missingLedger: false, drift, unrecorded: [] }));
      process.exit(1);
    }
    console.log(`[i18n-source-hashes] recorded in ${LEDGER}`);
    return;
  }
  const report = checkSourceHashes(root);
  const text = describeProblems(report);
  if (text) {
    console.error(text);
    process.exit(1);
  }
  console.log("[i18n-source-hashes] ok");
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
