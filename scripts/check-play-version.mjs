#!/usr/bin/env node
// PS5Upload Play carries PS5Upload's version (VERSION). The PS5 reads it from the app's
// sce_sys/param.json `contentVersion`, in its fixed NN.NNN.NNN form (13.0.0 is "13.000.000");
// scripts/update-version.js writes it. This gate fails when the two differ, like
// `make check-helper-version` does for the helper ELF.
//
//   node scripts/check-play-version.mjs                 the source (apps/play/sce_sys/param.json)
//   node scripts/check-play-version.mjs --zip <file>    a built release zip as well
//   node scripts/check-play-version.mjs --dist          the zip `make -C apps/play release` wrote
//
// The source check passes with a note while the app is not in the tree (no apps/play/). A zip
// must hold one top folder, FAKE99261/, with eboot.bin and sce_sys/param.json, and that param.json
// must say this title id and this version.

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";

export const TITLE_ID = "FAKE99261";
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** VERSION in the PS5's form (same rule as update-version.js); null when it has none. */
export function ps5ContentVersion(v) {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(String(v).trim());
  if (!m) return null;
  const [maj, min, pat] = [m[1], m[2], m[3]].map(Number);
  if (maj > 99 || min > 999 || pat > 999) return null;
  return `${String(maj).padStart(2, "0")}.${String(min).padStart(3, "0")}.${String(pat).padStart(3, "0")}`;
}

/** The problems with a param.json's text for this version (an empty list is a pass). */
export function paramProblems(text, want, where) {
  let p;
  try {
    p = JSON.parse(text);
  } catch (e) {
    return [`${where}: not JSON (${e.message})`];
  }
  const out = [];
  if (p.titleId !== TITLE_ID) out.push(`${where}: titleId is ${JSON.stringify(p.titleId)}, not ${TITLE_ID}`);
  if (p.contentVersion !== want) {
    out.push(
      `${where}: contentVersion is ${JSON.stringify(p.contentVersion)} but VERSION wants "${want}": run node scripts/update-version.js`,
    );
  }
  if (p.applicationCategoryType !== 0) {
    out.push(`${where}: applicationCategoryType is ${JSON.stringify(p.applicationCategoryType)}, not 0 (a game)`);
  }
  return out;
}

/** The entries of a zip (central directory): name -> {method, csize, size, offset}. */
export function zipEntries(buf) {
  let eocd = -1;
  for (let i = buf.length - 22; i >= Math.max(0, buf.length - 22 - 65535); i--) {
    if (buf.readUInt32LE(i) === 0x06054b50) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw new Error("not a zip (no end of central directory)");
  const count = buf.readUInt16LE(eocd + 10);
  let at = buf.readUInt32LE(eocd + 16);
  const out = new Map();
  for (let n = 0; n < count; n++) {
    if (buf.readUInt32LE(at) !== 0x02014b50) throw new Error("bad central directory entry");
    const method = buf.readUInt16LE(at + 10);
    const csize = buf.readUInt32LE(at + 20);
    const size = buf.readUInt32LE(at + 24);
    const nameLen = buf.readUInt16LE(at + 28);
    const extraLen = buf.readUInt16LE(at + 30);
    const commentLen = buf.readUInt16LE(at + 32);
    const offset = buf.readUInt32LE(at + 42);
    const name = buf.toString("utf8", at + 46, at + 46 + nameLen);
    out.set(name, { method, csize, size, offset });
    at += 46 + nameLen + extraLen + commentLen;
  }
  return out;
}

/** One entry's bytes (stored or deflated). */
export function zipRead(buf, e) {
  if (buf.readUInt32LE(e.offset) !== 0x04034b50) throw new Error("bad local header");
  const start = e.offset + 30 + buf.readUInt16LE(e.offset + 26) + buf.readUInt16LE(e.offset + 28);
  const data = buf.subarray(start, start + e.csize);
  if (e.method === 0) return Buffer.from(data);
  if (e.method === 8) return zlib.inflateRawSync(data);
  throw new Error(`compression method ${e.method} is not stored or deflate`);
}

/** The problems with a built zip for this version. */
export function zipProblems(buf, want, where) {
  let entries;
  try {
    entries = zipEntries(buf);
  } catch (e) {
    return [`${where}: ${e.message}`];
  }
  const out = [];
  const top = new Set([...entries.keys()].map((n) => n.split("/")[0]));
  if (top.size !== 1 || !top.has(TITLE_ID)) {
    out.push(`${where}: the top folder must be ${TITLE_ID}/ alone (has ${[...top].join(", ")})`);
  }
  for (const name of entries.keys()) {
    if (name.split("/").some((c) => c === ".." || c === "") && !name.endsWith("/")) {
      out.push(`${where}: unsafe entry name ${JSON.stringify(name)}`);
    }
  }
  if (!entries.has(`${TITLE_ID}/eboot.bin`)) out.push(`${where}: no ${TITLE_ID}/eboot.bin`);
  const param = entries.get(`${TITLE_ID}/sce_sys/param.json`);
  if (!param) {
    out.push(`${where}: no ${TITLE_ID}/sce_sys/param.json`);
    return out;
  }
  let text;
  try {
    text = zipRead(buf, param).toString("utf8");
  } catch (e) {
    return [...out, `${where}: param.json: ${e.message}`];
  }
  return [...out, ...paramProblems(text, want, `${where}: ${TITLE_ID}/sce_sys/param.json`)];
}

/** The release zip `make -C apps/play release` wrote, or null. */
export function distZip(root = repoRoot) {
  const dir = path.join(root, "apps", "play", "dist");
  let names = [];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return null;
  }
  const zips = names.filter((n) => n.startsWith(`PS5Upload-Play-${TITLE_ID}-`) && n.endsWith(".zip"));
  if (zips.length !== 1) return zips.length ? { many: zips.map((n) => path.join(dir, n)) } : null;
  return { file: path.join(dir, zips[0]) };
}

function main(argv) {
  const version = fs.readFileSync(path.join(repoRoot, "VERSION"), "utf8").trim();
  const want = ps5ContentVersion(version);
  if (!want) {
    console.error(`check-play-version: VERSION ${version} has no NN.NNN.NNN form`);
    return 1;
  }
  const problems = [];
  const appDir = path.join(repoRoot, "apps", "play");
  const paramPath = path.join(appDir, "sce_sys", "param.json");
  if (!fs.existsSync(appDir)) {
    console.log("check-play-version: no apps/play in this tree; nothing to check");
  } else if (!fs.existsSync(paramPath)) {
    problems.push("apps/play/sce_sys/param.json is missing");
  } else {
    problems.push(...paramProblems(fs.readFileSync(paramPath, "utf8"), want, "apps/play/sce_sys/param.json"));
  }

  let zip = null;
  const zi = argv.indexOf("--zip");
  if (zi >= 0) {
    if (!argv[zi + 1]) {
      console.error("check-play-version: --zip needs a file");
      return 2;
    }
    zip = argv[zi + 1];
  } else if (argv.includes("--dist")) {
    const d = distZip();
    if (!d) problems.push(`apps/play/dist has no PS5Upload-Play-${TITLE_ID}-*.zip`);
    else if (d.many) problems.push(`apps/play/dist has more than one zip: ${d.many.join(", ")}`);
    else zip = d.file;
  }
  if (zip) {
    let buf;
    try {
      buf = fs.readFileSync(zip);
    } catch (e) {
      problems.push(`${zip}: ${e.message}`);
    }
    if (buf) problems.push(...zipProblems(buf, want, path.basename(zip)));
  }

  if (problems.length) {
    for (const p of problems) console.error(`check-play-version: ${p}`);
    return 1;
  }
  console.log(`✓ check-play-version: PS5Upload Play is ${want} (VERSION ${version})${zip ? `, ${path.basename(zip)} too` : ""}`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exit(main(process.argv.slice(2)));
}
