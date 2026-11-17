#!/usr/bin/env node
// Reads `npm audit --json` on stdin and fails on any high or critical advisory that is not in
// scripts/npm-audit-allow.json (or whose allowance expired). The production dependencies are
// checked separately with --omit=dev and get no allowances (security-scan.sh).
import fs from "node:fs";
import path from "node:path";

const here = path.dirname(new URL(import.meta.url).pathname);
const allow = JSON.parse(fs.readFileSync(path.join(here, "npm-audit-allow.json"), "utf8")).allow ?? [];
const today = new Date().toISOString().slice(0, 10);
const live = new Map(allow.filter((a) => a.until >= today).map((a) => [a.id, a]));
const expired = allow.filter((a) => a.until < today);

const report = JSON.parse(fs.readFileSync(0, "utf8") || "{}");
const bad = [];
const allowed = new Set();
for (const [name, v] of Object.entries(report.vulnerabilities ?? {})) {
  if (!["high", "critical"].includes(v.severity)) continue;
  // A package flagged only because it depends on another flagged one: judged by that one.
  const own = (v.via ?? []).filter((x) => typeof x === "object");
  if (own.length === 0) continue;
  for (const adv of own) {
    if (!["high", "critical"].includes(adv.severity)) continue;
    const id = String(adv.url ?? "").split("/").pop();
    if (live.has(id)) allowed.add(`${id} (${name})`);
    else bad.push(`${name}: ${adv.title} ${adv.url} [${adv.severity}]`);
  }
}
for (const a of expired) console.log(`allowance expired on ${a.until}: ${a.id} (${a.package}) — look again`);
for (const a of allowed) console.log(`allowed (no fix yet, dev tools only): ${a}`);
if (bad.length || expired.length) {
  for (const b of bad) console.log(`not allowed: ${b}`);
  process.exit(1);
}
console.log("npm audit: nothing high or critical beyond the allowed list");
