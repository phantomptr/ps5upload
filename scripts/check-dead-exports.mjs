#!/usr/bin/env node
// Fails on exports in client/src nothing uses: no import anywhere, no use in their own file. They
// grow back by themselves (a hand clean-up in 2026-10 did not last), and they read as live code.
// Types and needless `export` on something its own file uses are not reported. Grep-based, with no
// dependency: it reads import statements, so a name built at runtime would be missed; add such a
// name to ALLOW with the reason.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const ROOT = path.join(REPO, "client/src");

/** `file:name` used in a way this script cannot see. */
const ALLOW = new Set([
  // Stand-ins vite aliases over the real modules in the small pad and join page builds.
  "client/src/join/joinLang.ts:useTr",
  "client/src/pad/langStub.ts:useTr",
  // Typed client for a live engine route; api/ps5.ts says why these stay without a screen.
  "client/src/api/ps5.ts:zipInspect",
  "client/src/api/ps5.ts:sevenzInspect",
]);

function walk(dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    if (["node_modules", "dist", "target", "gen"].includes(e.name)) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, out);
    else if (/\.(ts|tsx|mjs|js|cjs)$/.test(e.name) && !e.name.endsWith(".d.ts")) out.push(p);
  }
  return out;
}
const files = walk(ROOT);
const extra = [];
for (const e of ["client/scripts", "scripts", "tests", "site/scripts"]) {
  try { walk(path.join(REPO, e), extra); } catch {}
}
for (const e of fs.readdirSync(path.join(REPO, "client"))) {
  if (/^vite.*\.(ts|mjs|js)$/.test(e) || /^vitest.*\.(ts|mjs|js)$/.test(e)) extra.push(path.join(REPO, "client", e));
}
const src = new Map();
for (const f of [...files, ...extra]) src.set(f, fs.readFileSync(f, "utf8"));
const isTest = (f) => /\.test\.(ts|tsx)$/.test(f) || /test-setup|__repro/.test(f);

const EXP = /^export\s+(?:declare\s+)?(?:async\s+)?(function\*?|const|let|var|class|type|interface|enum)\s+([A-Za-z_$][\w$]*)/gm;
const strip = (s) => s
  .replace(/\/\*[\s\S]*?\*\//g, " ")
  .replace(/(^|[^:])\/\/.*$/gm, "$1");

// module specifier -> resolves to file
function resolve(from, spec) {
  if (!spec.startsWith(".")) return null;
  const base = path.resolve(path.dirname(from), spec.replace(/\.(js|ts|tsx|mjs)$/, ""));
  for (const c of [base + ".ts", base + ".tsx", base + ".mjs", base + ".js", path.join(base, "index.ts"), path.join(base, "index.tsx")]) {
    if (src.has(c)) return c;
  }
  return null;
}

// import graph: file -> { target -> Set(names) | '*' }
const importers = new Map(); // target -> Array<{from, names:Set, star:boolean}>
const IMPORT_RE = /(?:import|export)\s+(type\s+)?([^'";]*?)\s+from\s*["']([^"']+)["']/g;
const DYN_RE = /import\(\s*["']([^"']+)["']\s*\)/g;
for (const [f, s0] of src) {
  const s = strip(s0);
  let m;
  while ((m = IMPORT_RE.exec(s))) {
    const t = resolve(f, m[3]);
    if (!t) continue;
    const clause = m[2];
    const rec = { from: f, names: new Set(), star: false, reexport: /^export/.test(m[0]) && /^\s*\*\s*$/.test(clause) };
    if (/\*/.test(clause)) rec.star = true;
    const brace = clause.match(/\{([\s\S]*)\}/);
    if (brace) for (const part of brace[1].split(",")) {
      const nm = part.trim().replace(/^type\s+/, "").split(/\s+as\s+/)[0].trim();
      if (nm) rec.names.add(nm);
    }
    const def = clause.replace(/\{[\s\S]*\}/, "").replace(/,/g, "").trim();
    if (def && !def.startsWith("*")) rec.names.add("default");
    if (!importers.has(t)) importers.set(t, []);
    importers.get(t).push(rec);
  }
  while ((m = DYN_RE.exec(s))) {
    const t = resolve(f, m[1]);
    if (!t) continue;
    if (!importers.has(t)) importers.set(t, []);
    importers.get(t).push({ from: f, names: new Set(), star: true });
  }
}

/** Who uses `name` of `target`, following `export * from` to the importers of that file. */
function usersOf(target, name, seen = new Set()) {
  let prod = false;
  let test = false;
  if (seen.has(target)) return { prod, test };
  seen.add(target);
  const word = new RegExp(`(^|[^\\w$])${name.replace(/\$/g, "\\$")}(?![\\w$])`, "m");
  for (const r of importers.get(target) || []) {
    if (r.reexport) {
      const via = usersOf(r.from, name, seen);
      prod ||= via.prod;
      test ||= via.test;
      continue;
    }
    if (!(r.names.has(name) || (r.star && word.test(strip(src.get(r.from)))))) continue;
    if (isTest(r.from)) test = true;
    else prod = true;
  }
  return { prod, test };
}

const out = { dead: [], testOnly: [], internal: 0 };
for (const f of files) {
  if (isTest(f)) continue;
  const raw = src.get(f);
  const s = strip(raw);
  let m;
  EXP.lastIndex = 0;
  const seen = new Set();
  while ((m = EXP.exec(s))) {
    const [, kind, name] = m;
    if (seen.has(name)) continue;
    seen.add(name);
    const isType = kind === "type" || kind === "interface";
    const { prod, test } = usersOf(f, name);
    if (prod) continue;
    // own-file uses other than the declaration
    const uses = (s.match(new RegExp(`(^|\\.\\.\\.|[^\\w$.])${name.replace(/\$/g, "\\$")}(?![\\w$])`, "gm")) || []).length;
    const line = raw.slice(0, raw.indexOf(m[0]) < 0 ? 0 : raw.indexOf(m[0])).split("\n").length;
    const rel = path.relative(REPO, f);
    if (uses > 1) {
      if (!test) out.internal++;
      continue;
    }
    if (test) out.testOnly.push({ rel, line, name, isType });
    else out.dead.push({ rel, line, name, isType });
  }
}


const dead = out.dead.filter((x) => !x.isType && !ALLOW.has(`${x.rel}:${x.name}`));
if (dead.length > 0) {
  console.error("Exports nothing uses. Delete them, or say why in scripts/check-dead-exports.mjs:");
  for (const x of dead) console.error(`  ${x.rel}:${x.line} ${x.name}`);
  process.exit(1);
}
console.log(`dead exports: none (${out.testOnly.length} used only by tests)`);
