#!/usr/bin/env node
// Support & Thank you has one source: client/src/data/support.json (Ko-fi, the website's thanks
// section, the crypto addresses). The app reads it, the engine serves it at GET /api/support
// (support_api.rs, include_str!) for PS5Upload Play and Minion. README.md and the website carry
// copies a person reads; this fails when one of them no longer has every link and address, so a
// changed address can't be half rolled out.
import fs from "node:fs";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const read = (f) => fs.readFileSync(path.join(root, f), "utf8");
const support = JSON.parse(read("client/src/data/support.json"));

const problems = [];
if (!/^https:\/\/ko-fi\.com\/\w+$/.test(support.kofi ?? "")) problems.push("support.json: kofi is not a ko-fi.com link");
if (!Array.isArray(support.crypto) || support.crypto.length === 0) problems.push("support.json: no crypto addresses");

const copies = {
  "README.md": [support.kofi, ...support.crypto.map((c) => c.address)],
  "site/app.js": support.crypto.map((c) => c.address),
  "site/index.html": [support.kofi],
};
for (const [file, needles] of Object.entries(copies)) {
  const text = read(file);
  for (const n of needles) if (!text.includes(n)) problems.push(`${file}: missing ${n}`);
}
for (const c of support.crypto) {
  const svg = path.join(root, "site/assets/donate", `${c.id}.svg`);
  if (!fs.existsSync(svg)) problems.push(`site/assets/donate/${c.id}.svg: missing (the QR code of ${c.coin})`);
}

if (problems.length) {
  for (const p of problems) process.stderr.write(`support data: ${p}\n`);
  process.stderr.write("support data: edit client/src/data/support.json and every copy together.\n");
  process.exit(1);
}
process.stdout.write("support data: README and the website match support.json\n");
