#!/usr/bin/env node
// The website's translations (site/i18n/<code>.json) against its English (site/index.html's
// data-i18n and data-i18n-attr, and app.js's t("key", "English") calls; the same in the FAQ
// page's shell, scripts/site-faq.html, and its site/faq.js).
//
//   node scripts/site-i18n.mjs          check: every language has every key, no stale keys, the
//                                        same tags and {placeholders} as the English, and no
//                                        prose left in English
//   node scripts/site-i18n.mjs --write  also rewrite site/i18n/en.json, the English the
//                                        translators work from
//
// It also checks each language has its screenshots (site/assets/shots/<code>/).
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";

const site = new URL("../site/", import.meta.url).pathname;
const faqShell = new URL("./site-faq.html", import.meta.url).pathname;
const html = readFileSync(`${site}index.html`, "utf8") + readFileSync(faqShell, "utf8");
const js = readFileSync(`${site}app.js`, "utf8");
const scripts = js + readFileSync(`${site}faq.js`, "utf8");
const tidy = (s) => s.replace(/\s+/g, " ").trim();

const en = {};
const add = (key, value, where) => {
  if (key in en && en[key] !== value) throw new Error(`${key}: two different English texts (${where})`);
  en[key] = value;
};
for (const m of html.matchAll(/<(\w+)\b([^>]*?)\sdata-i18n="([^"]+)"([^>]*)>([\s\S]*?)<\/\1>/g)) {
  add(m[3], tidy(m[5]), "index.html");
}
for (const m of html.matchAll(/<\w+\b[^>]*\sdata-i18n-attr="([^"]+)"[^>]*>/g)) {
  for (const pair of m[1].split(";")) {
    const [attr, key] = pair.split(":");
    const v = m[0].match(new RegExp(`\\s${attr}="([^"]*)"`));
    if (!v) throw new Error(`${key}: the tag has no ${attr}`);
    // An attribute is set as plain text, so its entities are decoded here.
    const text = v[1].replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, '"').replace(/&amp;/g, "&");
    add(key, tidy(text), "index.html");
  }
}
for (const m of scripts.matchAll(/\bt\(\s*"([\w.]+)",\s*"((?:[^"\\]|\\.)*)"/g)) add(m[1], JSON.parse(`"${m[2]}"`), "app.js or faq.js");

const LANGS = [...js.matchAll(/^\s*\["([\w-]+)", "[^"]+"\],$/gm)].map((m) => m[1]);
if (LANGS.length !== 21) throw new Error(`expected 21 languages in app.js, found ${LANGS.length}`);
const SHOTS = readdirSync(`${site}assets/shots`).filter((f) => f.endsWith(".webp"));

if (process.argv.includes("--write")) {
  const sorted = Object.fromEntries(Object.keys(en).sort().map((k) => [k, en[k]]));
  writeFileSync(`${site}i18n/en.json`, `${JSON.stringify(sorted, null, 2)}\n`);
}

/** What has to survive translation: tags (with their attributes) and {placeholders}. */
const shape = (s) => [...s.matchAll(/<[^>]+>|\{\w+\}/g)].map((m) => m[0]).sort().join(" ");

/** Two words of 3+ letters: a translator would be expected to change it (same test as i18n-coverage.mjs). */
const isProse = (s) => (s.match(/[A-Za-z]{3,}/g) ?? []).length >= 2;

/** English text that is right as it is. Distro names don't translate; "portable" is also the es and fr word. */
const SAME_AS_ENGLISH = {
  "dl.linux_arm64_deb": "*",
  "dl.linux_arm64_rpm": "*",
  "dl.linux_x64_deb": "*",
  "dl.linux_x64_rpm": "*",
  "dl.win_arm64_zip": ["es", "fr"],
};
const mayStayEnglish = (key, code) => {
  const allowed = SAME_AS_ENGLISH[key];
  return allowed === "*" || (Array.isArray(allowed) && allowed.includes(code));
};

let bad = 0;
const fail = (msg) => {
  bad++;
  console.error(msg);
};
const saved = JSON.parse(readFileSync(`${site}i18n/en.json`, "utf8"));
for (const k of new Set([...Object.keys(en), ...Object.keys(saved)])) {
  if (saved[k] !== en[k]) fail(`en.json is out of date at ${k} (run with --write)`);
}
for (const code of LANGS.filter((c) => c !== "en")) {
  const file = `${site}i18n/${code}.json`;
  if (!existsSync(file)) {
    fail(`${code}: no ${file}`);
    continue;
  }
  const tr = JSON.parse(readFileSync(file, "utf8"));
  for (const k of Object.keys(en)) {
    if (typeof tr[k] !== "string" || !tr[k].trim()) fail(`${code}: missing ${k}`);
    else if (shape(tr[k]) !== shape(en[k])) fail(`${code}: ${k} has other tags or placeholders than the English`);
    else if (tr[k] === en[k] && isProse(en[k]) && !mayStayEnglish(k, code)) fail(`${code}: ${k} is still English: ${en[k]}`);
  }
  for (const k of Object.keys(tr)) if (!(k in en)) fail(`${code}: ${k} is not used any more`);
  const missing = SHOTS.filter((f) => !existsSync(`${site}assets/shots/${code}/${f}`));
  if (missing.length) fail(`${code}: ${missing.length} screenshots missing (${missing.slice(0, 3).join(", ")}…)`);
}
console.log(`${Object.keys(en).length} strings, ${LANGS.length} languages${bad ? `: ${bad} problems` : ": all there"}`);
process.exit(bad ? 1 : 0);
