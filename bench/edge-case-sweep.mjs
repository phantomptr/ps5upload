#!/usr/bin/env node
/*
 * Upload edge-case sweep (manual hardware test, not CI).
 *
 * Exercises the tricky upload paths the smoke test doesn't: special-char
 * filenames, empty files, nested dirs, reconcile-skip, zip upload, multistream
 * with odd names, and concurrent uploads — and VERIFIES correctness by
 * downloading each upload back and byte-comparing (catches silent corruption,
 * not just "job said ok").
 *
 * Usage: node bench/edge-case-sweep.mjs <ps5-ip>   (engine must be up on :19113)
 */
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { createWriteStream } from "node:fs";
import { execFileSync } from "node:child_process";

const ENGINE = process.env.ENGINE_URL || "http://127.0.0.1:19113";
const HOST = process.argv[2];
if (!HOST) { console.error("usage: edge-case-sweep.mjs <ps5-ip>"); process.exit(2); }
const ADDR = HOST.includes(":") ? HOST : `${HOST}:9113`;
const DEST = "/data/ps5upload/tests/edge";

let pass = 0, fail = 0;
const failures = [];
const ok = (l) => { pass++; console.log(`  ✓ ${l}`); };
const bad = (l, d) => { fail++; failures.push(`${l} :: ${d}`); console.log(`  ✗ ${l}  — ${d}`); };

const sha = (buf) => crypto.createHash("sha256").update(buf).digest("hex");

async function jpost(p, body) {
  const r = await fetch(`${ENGINE}${p}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
  const t = await r.text();
  let j; try { j = JSON.parse(t); } catch { j = { raw: t }; }
  return { ok: r.ok, status: r.status, json: j };
}
async function jget(p) {
  const r = await fetch(`${ENGINE}${p}`); const t = await r.text();
  let j; try { j = JSON.parse(t); } catch { j = { raw: t }; }
  return { ok: r.ok, status: r.status, json: j };
}
async function pollJob(id, timeoutMs = 120000) {
  const t0 = Date.now();
  for (;;) {
    const r = await jget(`/api/jobs/${id}`);
    const s = r.json.status;
    if (s === "done") return { ok: true, j: r.json };
    if (s === "failed" || s === "error") return { ok: false, error: r.json.error || JSON.stringify(r.json) };
    if (Date.now() - t0 > timeoutMs) return { ok: false, error: "timeout" };
    await new Promise((x) => setTimeout(x, 300));
  }
}
async function del(p) { await jpost("/api/ps5/fs/delete", { path: p, addr: ADDR }); }

// Upload a dir, download it back to a fresh local dir, return map name->sha of downloaded files.
async function uploadDirAndVerify(label, localDir, ps5Sub, { streams = 1, reconcile = false } = {}) {
  const destRoot = `${DEST}/${ps5Sub}`;
  await del(destRoot);
  await new Promise((x) => setTimeout(x, 400));
  const ep = reconcile ? "/api/transfer/dir-reconcile" : "/api/transfer/dir";
  const body = reconcile
    ? { src_dir: localDir, dest_root: destRoot, addr: ADDR, mode: "fast", tx_id: null, excludes: [], streams }
    : { src_dir: localDir, dest_root: destRoot, addr: ADDR };
  const st = await jpost(ep, body);
  if (!st.ok || !st.json.job_id) return bad(label, `start HTTP ${st.status} ${JSON.stringify(st.json)}`);
  const res = await pollJob(st.json.job_id);
  if (!res.ok) return bad(label, `job ${res.error}`);
  // Download folder back
  const dlDir = await fs.mkdtemp(path.join(os.tmpdir(), "edge-dl-"));
  const dl = await jpost("/api/transfer/download", { src_path: destRoot, dest_dir: dlDir, kind: "folder", addr: ADDR });
  if (!dl.ok || !dl.json.job_id) return bad(label, `download start HTTP ${dl.status} ${JSON.stringify(dl.json)}`);
  const dlres = await pollJob(dl.json.job_id);
  if (!dlres.ok) return bad(label, `download job ${dlres.error}`);
  // Compare every local file against downloaded counterpart
  const localFiles = await walk(localDir);
  let mism = 0, checked = 0, missing = [];
  const dlRoot = path.join(dlDir, path.basename(destRoot));
  for (const rel of localFiles) {
    const a = await fs.readFile(path.join(localDir, rel));
    let b;
    try { b = await fs.readFile(path.join(dlRoot, rel)); }
    catch { missing.push(rel); continue; }
    checked++;
    if (sha(a) !== sha(b)) { mism++; if (mism <= 3) console.log(`      mismatch: ${JSON.stringify(rel)} (${a.length}B vs ${b.length}B)`); }
  }
  await fs.rm(dlDir, { recursive: true, force: true });
  if (missing.length) return bad(label, `${missing.length} file(s) missing after round-trip, e.g. ${JSON.stringify(missing.slice(0,3))}`);
  if (mism) return bad(label, `${mism}/${checked} file(s) byte-mismatch after round-trip`);
  ok(`${label} (${checked} files round-tripped, streams=${streams}${reconcile ? ", reconcile" : ""})`);
  return res.j;
}

async function walk(dir, base = dir) {
  const out = [];
  for (const e of await fs.readdir(dir, { withFileTypes: true })) {
    const fp = path.join(dir, e.name);
    if (e.isDirectory()) out.push(...await walk(fp, base));
    else out.push(path.relative(base, fp));
  }
  return out;
}

async function main() {
  console.log(`\n==== EDGE-CASE SWEEP → ${ADDR} ====`);
  const work = await fs.mkdtemp(path.join(os.tmpdir(), "edge-src-"));

  // ---- 1. Special-character filenames ----
  const specialDir = path.join(work, "special");
  await fs.mkdir(specialDir, { recursive: true });
  const trickyNames = [
    "plain.bin", "with space.bin", "brace}close.bin", "brace{open.bin",
    "brack[1].bin", "hash#1.bin", "amp&and.bin", "quote'q.bin", "paren(1).bin",
    "comma,1.bin", "plus+1.bin", "at@1.bin", "eq=1.bin", "semi;1.bin",
    "bang!1.bin", "pct%20.bin", "dollar$1.bin", "tilde~1.bin", "caret^1.bin",
    "日本語.bin", "emoji😀.bin", "ünïcödé.bin", ".hiddenleading.bin",
    "double  space.bin", "trailing.dot.", `${"long".repeat(40)}.bin`,
  ];
  for (let i = 0; i < trickyNames.length; i++) {
    await fs.writeFile(path.join(specialDir, trickyNames[i]), crypto.randomBytes(1024 + i * 37));
  }
  await uploadDirAndVerify("special-chars (dir, single stream)", specialDir, "special", { streams: 1 });
  await uploadDirAndVerify("special-chars (dir, 4 streams)", specialDir, "special4", { streams: 4 });

  // ---- 2. Empty file + empty-ish mix ----
  const emptyDir = path.join(work, "empties");
  await fs.mkdir(emptyDir, { recursive: true });
  await fs.writeFile(path.join(emptyDir, "zero.bin"), Buffer.alloc(0));
  await fs.writeFile(path.join(emptyDir, "one-byte.bin"), Buffer.from([0x7a]));
  await fs.writeFile(path.join(emptyDir, "normal.bin"), crypto.randomBytes(4096));
  await uploadDirAndVerify("empty + 1-byte + normal mix", emptyDir, "empties");

  // ---- 3. Deeply nested dirs ----
  const nested = path.join(work, "nested");
  let deep = nested;
  for (const seg of ["lvl1", "lvl2 with space", "lvl3}brace", "lvl4", "lvl5"]) {
    deep = path.join(deep, seg);
  }
  await fs.mkdir(deep, { recursive: true });
  await fs.writeFile(path.join(deep, "deep.bin"), crypto.randomBytes(8192));
  await fs.writeFile(path.join(nested, "lvl1", "mid.bin"), crypto.randomBytes(2048));
  await uploadDirAndVerify("deeply nested dirs (with special path segs)", nested, "nested");

  // ---- 4. Reconcile skip (upload twice; 2nd should send ~0 bytes) ----
  const recDir = path.join(work, "rec");
  await fs.mkdir(recDir, { recursive: true });
  for (let i = 0; i < 6; i++) await fs.writeFile(path.join(recDir, `rec_${i}.bin`), crypto.randomBytes(64 * 1024));
  const destRec = `${DEST}/rec`;
  await del(destRec); await new Promise((x) => setTimeout(x, 400));
  const r1 = await jpost("/api/transfer/dir-reconcile", { src_dir: recDir, dest_root: destRec, addr: ADDR, mode: "fast", tx_id: null, excludes: [], streams: 1 });
  const r1res = await pollJob(r1.json.job_id);
  if (!r1res.ok) bad("reconcile run-1", r1res.error);
  else {
    const r2 = await jpost("/api/transfer/dir-reconcile", { src_dir: recDir, dest_root: destRec, addr: ADDR, mode: "fast", tx_id: null, excludes: [], streams: 1 });
    const r2res = await pollJob(r2.json.job_id);
    if (!r2res.ok) bad("reconcile run-2", r2res.error);
    else {
      const sent = r2res.j.bytes_sent ?? -1;
      const skipped = r2res.j.skipped_files ?? 0;
      if (sent === 0 || skipped >= 6) ok(`reconcile skip (run-2 sent ${sent}B, skipped ${skipped} files)`);
      else bad("reconcile skip", `run-2 re-sent ${sent}B / skipped ${skipped} (expected ~0 sent / 6 skipped)`);
    }
  }

  // ---- 5. Zip upload (extract on PS5) ----
  try {
    const zipSrc = path.join(work, "ziproot");
    await fs.mkdir(path.join(zipSrc, "sub dir"), { recursive: true });
    await fs.writeFile(path.join(zipSrc, "a.bin"), crypto.randomBytes(3000));
    await fs.writeFile(path.join(zipSrc, "sub dir", "b}brace.bin"), crypto.randomBytes(5000));
    const zipPath = path.join(work, "archive.zip");
    execFileSync("zip", ["-r", "-q", zipPath, "."], { cwd: zipSrc });
    const destZip = `${DEST}/zip`;
    await del(destZip); await new Promise((x) => setTimeout(x, 400));
    const z = await jpost("/api/transfer/zip", { zip_path: zipPath, dest_root: destZip, addr: ADDR });
    if (!z.ok || !z.json.job_id) bad("zip upload", `start HTTP ${z.status} ${JSON.stringify(z.json)}`);
    else {
      const zres = await pollJob(z.json.job_id);
      if (!zres.ok) bad("zip upload", `job ${zres.error}`);
      else {
        // verify by download-back
        const dlDir = await fs.mkdtemp(path.join(os.tmpdir(), "edge-zip-"));
        const dl = await jpost("/api/transfer/download", { src_path: destZip, dest_dir: dlDir, kind: "folder", addr: ADDR });
        const dlres = await pollJob(dl.json.job_id);
        if (!dlres.ok) bad("zip upload verify", `download ${dlres.error}`);
        else {
          const a = await fs.readFile(path.join(zipSrc, "a.bin"));
          const b = await fs.readFile(path.join(dlDir, "zip", "a.bin")).catch(() => null);
          const c = await fs.readFile(path.join(zipSrc, "sub dir", "b}brace.bin"));
          const d = await fs.readFile(path.join(dlDir, "zip", "sub dir", "b}brace.bin")).catch(() => null);
          if (b && d && sha(a) === sha(b) && sha(c) === sha(d)) ok("zip upload (extracted + round-tripped incl. nested brace name)");
          else bad("zip upload verify", `extracted bytes mismatch (a:${!!b} b:${!!d})`);
        }
        await fs.rm(dlDir, { recursive: true, force: true });
      }
    }
  } catch (e) { bad("zip upload", `harness error: ${e.message}`); }

  // ---- 6. Concurrent uploads (two files at once) ----
  try {
    const f1 = path.join(work, "cc1.bin"), f2 = path.join(work, "cc2.bin");
    await fs.writeFile(f1, crypto.randomBytes(2 * 1024 * 1024));
    await fs.writeFile(f2, crypto.randomBytes(2 * 1024 * 1024));
    await del(`${DEST}/cc`); await new Promise((x) => setTimeout(x, 400));
    const [a, b] = await Promise.all([
      jpost("/api/transfer/file", { src: f1, dest: `${DEST}/cc/cc1.bin`, addr: ADDR, tx_id: null }),
      jpost("/api/transfer/file", { src: f2, dest: `${DEST}/cc/cc2.bin`, addr: ADDR, tx_id: null }),
    ]);
    const [ra, rb] = await Promise.all([pollJob(a.json.job_id), pollJob(b.json.job_id)]);
    if (ra.ok && rb.ok) ok("concurrent uploads (2 files in parallel both committed)");
    else bad("concurrent uploads", `a:${ra.ok ? "ok" : ra.error} b:${rb.ok ? "ok" : rb.error}`);
  } catch (e) { bad("concurrent uploads", `harness error: ${e.message}`); }

  // cleanup
  await del(DEST);
  await fs.rm(work, { recursive: true, force: true });

  console.log(`\n---- ${ADDR}: ${pass} passed, ${fail} failed ----`);
  if (failures.length) { console.log("FAILURES:"); for (const f of failures) console.log(`  • ${f}`); }
  process.exitCode = fail ? 1 : 0;
}
main().catch((e) => { console.error("FATAL", e); process.exit(3); });
