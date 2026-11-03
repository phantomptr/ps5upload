#!/usr/bin/env node
// The Thank you page's list (Help › Thank you, and README's Supporters section) from a Ko-fi
// transactions export. Run it on your own computer:
//
//   node scripts/supporters.mjs ~/Downloads/Transaction_All.csv
//
// The export never goes into the repository (.gitignore keeps Ko-fi exports out). What is
// written holds only each supporter's display name and tier: no amount, email, message, date,
// country or transaction id. A tier is decided from a person's total here and only its name is
// kept. Tips under Ko-fi's anonymous names ("Ko-fi Supporter", "Supporter", none) are only
// counted. Your own test shop orders (from PhantomPtr) are left out.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(ROOT, "client/src/data/supporters.json");
const README = join(ROOT, "README.md");
const OWNER = "phantomptr";
const ANONYMOUS = new Set(["", "ko-fi supporter", "supporter", "someone", "anonymous"]);
/** Tiers by a person's total, highest first (the amounts stay in this script only). */
const TIERS = [
  ["champion", 20],
  ["hero", 10],
  ["friend", 5],
  ["supporter", 0],
];
const TIER_TITLES = { champion: "Champions", hero: "Heroes", friend: "Friends", supporter: "Supporters" };
const TIER_DOTS = { champion: "🟡", hero: "🟣", friend: "🔵", supporter: "🟢" };

/** RFC 4180 CSV: quoted fields may hold commas, quotes ("") and line breaks. */
function parseCsv(text) {
  const rows = [];
  let row = [];
  let field = "";
  let quoted = false;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') {
        field += '"';
        i++;
      } else if (c === '"') quoted = false;
      else field += c;
    } else if (c === '"') quoted = true;
    else if (c === ",") {
      row.push(field);
      field = "";
    } else if (c === "\n" || c === "\r") {
      if (c === "\r" && text[i + 1] === "\n") i++;
      row.push(field);
      rows.push(row);
      row = [];
      field = "";
    } else field += c;
  }
  if (field || row.length) {
    row.push(field);
    rows.push(row);
  }
  return rows.filter((r) => r.some((f) => f.trim()));
}

const path = process.argv[2];
if (!path) {
  console.error("usage: node scripts/supporters.mjs <Ko-fi Transaction_All.csv>");
  process.exit(2);
}
const [header, ...data] = parseCsv(readFileSync(path, "utf8").replace(/^﻿/, ""));
const col = (name) => {
  const i = header.indexOf(name);
  if (i < 0) throw new Error(`the export has no "${name}" column`);
  return i;
};
const FROM = col("From");
const RECEIVED = col("Received");
const TYPE = col("TransactionType");

const totals = new Map();
let anonymous = 0;
for (const r of data) {
  const from = (r[FROM] ?? "").trim();
  const amount = Number.parseFloat(r[RECEIVED] ?? "") || 0;
  const type = (r[TYPE] ?? "").toLowerCase();
  if (from.toLowerCase() === OWNER || amount <= 0) continue;
  if (!type.includes("tip") && !type.includes("donation") && !type.includes("subscription")) continue;
  if (ANONYMOUS.has(from.toLowerCase())) {
    anonymous++;
    continue;
  }
  totals.set(from, (totals.get(from) ?? 0) + amount);
}

const tiers = Object.fromEntries(TIERS.map(([t]) => [t, []]));
for (const [name, total] of totals) {
  const [tier] = TIERS.find(([, min]) => total >= min);
  tiers[tier].push(name);
}
for (const t of Object.keys(tiers)) tiers[t].sort((a, b) => a.localeCompare(b, "en", { sensitivity: "base" }));

writeFileSync(OUT, `${JSON.stringify({ tiers, anonymous }, null, 2)}\n`);

// README: the same names between the markers.
const lines = ["<!-- supporters:start (scripts/supporters.mjs) -->"];
for (const [t] of TIERS) {
  if (!tiers[t].length) continue;
  lines.push(`**${TIER_DOTS[t]} ${TIER_TITLES[t]}:** ${tiers[t].join(" · ")}`, "");
}
if (anonymous) lines.push(`…and ${anonymous} anonymous ${anonymous === 1 ? "supporter" : "supporters"}. Thank you all! 💛`, "");
lines.push("<!-- supporters:end -->");
const readme = readFileSync(README, "utf8");
const block = /<!-- supporters:start[\s\S]*?<!-- supporters:end -->/;
if (!block.test(readme)) throw new Error("README.md has no supporters markers");
writeFileSync(README, readme.replace(block, lines.join("\n")));

const n = Object.values(tiers).reduce((a, l) => a + l.length, 0);
console.log(`${n} named supporters, ${anonymous} anonymous tips → client/src/data/supporters.json and README.md`);
