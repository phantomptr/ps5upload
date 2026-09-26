#!/usr/bin/env node
/*
 * Multi-stream hardware test harness (manual, not CI).
 *
 * Drives the ps5upload engine HTTP API against a live PS5 to measure upload
 * throughput at varying parallel-stream counts. Assumes the engine is already
 * running (spawn it separately so it survives across subcommands and its logs
 * are visible). Sequential by design — one upload at a time.
 *
 * Subcommands:
 *   status <host>                          print STATUS (incl max_transfer_streams)
 *   delete <host> <ps5path>                rm -rf a PS5 path (best effort)
 *   dir    <host> <src> <destRoot> <csv>   folder upload at each stream count in <csv>
 *   file   <host> <src> <ps5dest>          single-file upload (single stream)
 *
 * Env: ENGINE_URL (default http://127.0.0.1:19113)
 */
const ENGINE = process.env.ENGINE_URL || "http://127.0.0.1:19113";
const MGMT = (h) => (h.includes(":") ? h : `${h}:9113`); // engine derives mgmt from this

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const mib = (b) => b / (1024 * 1024);

async function jpost(path, body) {
  const r = await fetch(`${ENGINE}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await r.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = { raw: text };
  }
  return { ok: r.ok, status: r.status, json };
}

async function jget(path) {
  const r = await fetch(`${ENGINE}${path}`);
  const text = await r.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = { raw: text };
  }
  return { ok: r.ok, status: r.status, json };
}

async function cmdStatus(host) {
  const r = await jget(`/api/ps5/status?addr=${encodeURIComponent(MGMT(host))}`);
  console.log(`STATUS ${host}: HTTP ${r.status}`);
  console.log(JSON.stringify(r.json, null, 2));
}

async function cmdDelete(host, ps5path) {
  const r = await jpost("/api/ps5/fs/delete", { addr: MGMT(host), path: ps5path, op_id: 0 });
  console.log(`delete ${ps5path}: HTTP ${r.status} ${JSON.stringify(r.json)}`);
}

// Poll a job to completion, sampling bytes_sent to estimate peak instantaneous
// rate. Returns { ok, elapsedMs, bytes, shards, files, peakMiBs, error }.
async function pollJob(jobId, { sampleMs = 500 } = {}) {
  let last = { t: Date.now(), bytes: 0 };
  let peak = 0;
  for (;;) {
    const r = await jget(`/api/jobs/${jobId}`);
    const j = r.json || {};
    const status = j.status;
    if (status === "running") {
      const now = Date.now();
      const bytes = j.bytes_sent || 0;
      const dt = (now - last.t) / 1000;
      if (dt >= 0.4) {
        const inst = mib(bytes - last.bytes) / dt;
        if (inst > peak) peak = inst;
        last = { t: now, bytes };
      }
      await sleep(sampleMs);
      continue;
    }
    if (status === "done") {
      return {
        ok: true,
        elapsedMs: j.elapsed_ms || 0,
        bytes: j.bytes_sent || 0,
        shards: j.shards_sent || 0,
        files: j.files_sent || 0,
        peakMiBs: peak,
      };
    }
    if (status === "failed") {
      return { ok: false, error: j.error || JSON.stringify(j) };
    }
    // Unknown / queued — keep polling.
    await sleep(sampleMs);
  }
}

async function cmdDir(host, src, destRoot, csv) {
  const streamsList = csv.split(",").map((s) => parseInt(s.trim(), 10)).filter((n) => n >= 1);
  const results = [];
  for (const streams of streamsList) {
    // Clean the dest so every run re-sends the full folder (reconcile would
    // otherwise skip already-present files and report ~0 bytes on run 2+).
    await cmdDelete(host, destRoot);
    await sleep(1500); // let the payload settle after the delete
    console.log(`\n=== dir upload: streams=${streams} → ${destRoot} ===`);
    const t0 = Date.now();
    const start = await jpost("/api/transfer/dir-reconcile", {
      src_dir: src,
      dest_root: destRoot,
      addr: MGMT(host),
      mode: "fast",
      tx_id: null,
      excludes: [],
      streams,
    });
    if (!start.ok || !start.json.job_id) {
      console.log(`  START FAILED: HTTP ${start.status} ${JSON.stringify(start.json)}`);
      results.push({ streams, ok: false, error: `start ${start.status}` });
      continue;
    }
    const res = await pollJob(start.json.job_id);
    const wallMs = Date.now() - t0;
    if (!res.ok) {
      console.log(`  FAILED: ${res.error}`);
      results.push({ streams, ok: false, error: res.error });
      continue;
    }
    const avg = mib(res.bytes) / (res.elapsedMs / 1000);
    const avgWall = mib(res.bytes) / (wallMs / 1000);
    console.log(
      `  OK ${(mib(res.bytes)).toFixed(0)} MiB / ${(res.elapsedMs / 1000).toFixed(1)}s` +
        ` | avg ${avg.toFixed(1)} MiB/s (job) ${avgWall.toFixed(1)} MiB/s (wall)` +
        ` | peak ${res.peakMiBs.toFixed(1)} MiB/s | files=${res.files} shards=${res.shards}`,
    );
    results.push({ streams, ok: true, bytes: res.bytes, elapsedMs: res.elapsedMs, avg, peak: res.peakMiBs });
    await sleep(2000);
  }
  console.log(`\n── summary (${host}) ──`);
  for (const r of results) {
    if (r.ok) console.log(`  streams=${r.streams}: avg ${r.avg.toFixed(1)} MiB/s, peak ${r.peak.toFixed(1)} MiB/s`);
    else console.log(`  streams=${r.streams}: FAILED ${r.error}`);
  }
}

async function cmdFile(host, src, dest) {
  console.log(`\n=== single-file upload → ${dest} ===`);
  const t0 = Date.now();
  const start = await jpost("/api/transfer/file", {
    src,
    dest,
    addr: MGMT(host),
    tx_id: null,
  });
  if (!start.ok || !start.json.job_id) {
    console.log(`  START FAILED: HTTP ${start.status} ${JSON.stringify(start.json)}`);
    return;
  }
  const res = await pollJob(start.json.job_id);
  const wallMs = Date.now() - t0;
  if (!res.ok) {
    console.log(`  FAILED: ${res.error}`);
    return;
  }
  const avg = mib(res.bytes) / (res.elapsedMs / 1000);
  console.log(
    `  OK ${(mib(res.bytes)).toFixed(0)} MiB / ${(res.elapsedMs / 1000).toFixed(1)}s` +
      ` | avg ${avg.toFixed(1)} MiB/s | peak ${res.peakMiBs.toFixed(1)} MiB/s | wall ${(wallMs / 1000).toFixed(1)}s`,
  );
}

const [cmd, ...args] = process.argv.slice(2);
const run = {
  status: () => cmdStatus(args[0]),
  delete: () => cmdDelete(args[0], args[1]),
  dir: () => cmdDir(args[0], args[1], args[2], args[3]),
  file: () => cmdFile(args[0], args[1], args[2]),
};
if (!run[cmd]) {
  console.error("usage: status|delete|dir|file (see header)");
  process.exit(2);
}
run[cmd]().catch((e) => {
  console.error(e);
  process.exit(1);
});
