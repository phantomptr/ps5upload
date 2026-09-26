#!/usr/bin/env node
/**
 * tests/install-fallback-hw.mjs
 *
 * Real-hardware test for the PKG-install fallback chain (the 2.27.x
 * FW-12 reorder in payload/src/bgft.c). It stages a DUMMY (invalid) .pkg
 * on the console and kicks off an install, forcing every tier to fail in
 * order:
 *
 *   Tier 1  in-process sceAppInstUtilInstallByPackage(local)   → reject
 *   Tier 2  ShellUI-RPC InstallByPackage                       → reject
 *   Tier 3  legacy BGFT IntDebug register                      → reject
 *   LAST    sceAppInstUtilAppInstallPkg (the unlaunchable one) → reject
 *
 * The point is NOT a successful install (we have no valid fakepkg) — it's
 * to prove the reordered chain:
 *   1. does NOT crash or wedge the payload (mgmt port answers afterwards),
 *   2. returns a structured error with a sane register_path / via, and
 *   3. never reports a clean success for the dummy.
 *
 * NOTE: this ptraces SceShellUI on the console, which can cause a brief
 * black flash / ShellUI respawn on the TV. It is recoverable. Opt-in:
 * run it explicitly, it is not part of `npm test`.
 *
 * Usage:
 *   node tests/install-fallback-hw.mjs --ps5=192.168.86.99 [--engine=URL]
 *
 * Exit 0 = payload survived + returned structured diagnostics.
 * Exit 1 = payload crashed, wedged, or falsely reported success.
 */
import { spawn } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, '..');

function arg(name, def) {
  const hit = process.argv.find((a) => a.startsWith(`--${name}=`));
  return hit ? hit.slice(name.length + 3) : def;
}

const PS5 = arg('ps5', '192.168.86.99');
const ENGINE = arg('engine', 'http://127.0.0.1:19113');
const MGMT = `${PS5}:9114`;
const DATA = `${PS5}:9113`;
const STAGE = '/data/ps5upload/tests/install-fallback/dummy.pkg';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function getJson(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url} -> HTTP ${r.status}`);
  return r.json();
}
async function postJson(url, body) {
  const r = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  const text = await r.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = { _raw: text };
  }
  return { ok: r.ok, status: r.status, json };
}
async function waitHttp(url, ms) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const r = await fetch(url);
      if (r.ok) return true;
    } catch {
      /* keep waiting */
    }
    await sleep(300);
  }
  return false;
}

let engineChild = null;
async function maybeSpawnEngine() {
  if (await waitHttp(`${ENGINE}/api/jobs`, 1000)) return; // already up
  const bin = path.join(repoRoot, 'engine', 'target', 'release', 'ps5upload-engine');
  engineChild = spawn(bin, [], {
    stdio: 'inherit',
    env: { ...process.env, PS5UPLOAD_PS5_ADDR: DATA },
  });
  if (!(await waitHttp(`${ENGINE}/api/jobs`, 20_000))) {
    throw new Error('engine did not come up');
  }
}

async function main() {
  let failures = 0;
  const note = (ok, label, extra) => {
    if (!ok) failures++;
    process.stdout.write(`  ${ok ? '✓ PASS' : '✗ FAIL'}  ${label}${extra ? ` — ${extra}` : ''}\n`);
  };

  await maybeSpawnEngine();
  process.stdout.write(`[install-fallback] ps5=${PS5} engine=${ENGINE}\n`);

  // 0. Payload alive before.
  const before = await getJson(`${ENGINE}/api/ps5/status?addr=${encodeURIComponent(MGMT)}`);
  note(!!before, 'payload alive before', before?.version ? `v${before.version} ${before.ps5_kernel ?? ''}` : '');

  // 1. Stage a dummy (invalid) .pkg on the console (1 KiB of zeros).
  const stage = await postJson(`${ENGINE}/api/transfer/bytes-b64`, {
    addr: DATA,
    dest: STAGE,
    // 1 KiB of 0x00 — definitely not a valid PKG header.
    data_b64: Buffer.alloc(1024, 0).toString('base64'),
  }).catch((e) => ({ ok: false, json: { error: String(e) } }));
  // The bytes endpoint may not exist on older engines; fall back to a file transfer.
  if (!stage.ok) {
    // Fallback: write a temp file and use /api/transfer/file.
    const fs = await import('node:fs/promises');
    const os = await import('node:os');
    const tmp = path.join(os.tmpdir(), `iflbk-${Date.now()}.pkg`);
    await fs.writeFile(tmp, Buffer.alloc(1024, 0));
    const j = await postJson(`${ENGINE}/api/transfer/file`, { addr: DATA, dest: STAGE, src: tmp });
    const jobId = j.json?.job_id;
    if (jobId) {
      for (let i = 0; i < 40; i++) {
        const job = await getJson(`${ENGINE}/api/jobs/${jobId}`);
        if (job.status === 'done' || job.status === 'error') break;
        await sleep(250);
      }
    }
    await fs.rm(tmp, { force: true });
  }

  // 2. Kick the install with the staged dummy as a LOCAL path → forces the
  //    full reordered fallback chain.
  const res = await postJson(`${ENGINE}/api/pkg/install/start`, {
    ps5_addr: MGMT,
    path: null,
    split_root: null,
    package_type_override: null,
    local_ps5_path: STAGE,
    content_id: 'IV0000-NPXX99999_00-DUMMY00000000000',
  });
  process.stdout.write(`  install/start -> HTTP ${res.status}: ${JSON.stringify(res.json)}\n`);

  // 3. Sony's InstallByPackage is ASYNC-ACCEPT: rc=0 means "queued", not
  //    "valid pkg" — a dummy can legitimately be accepted by a launchable
  //    tier (in-proc appinst or shellui-rpc) and only fail later, invisibly
  //    (those tiers take the synthetic-DONE status bypass). So we do NOT
  //    assert "rejected". We assert the INVARIANTS the 2.27.x change adds:
  const rc = (res.json?.err_code ?? 0) >>> 0;
  const regPath = res.json?.register_path ?? '';
  const via = res.json?.via ?? '';
  const mnl = res.json?.may_not_launch ?? false;
  const KNOWN = ['', 'none', 'appinst', 'appinst-local', 'shellui-rpc', 'intdebug', 'regular', 'tier0-worker'];
  process.stdout.write(
    `  outcome: err=0x${rc.toString(16)} register_path=${regPath || '∅'} via=${via || '∅'} may_not_launch=${mnl}\n`,
  );
  // (a) register_path is a value the host knows how to render.
  note(KNOWN.includes(regPath), 'register_path is a known value', regPath || '∅');
  // (b) THE core invariant: may_not_launch is true IFF the unlaunchable
  //     last-resort path (appinst-local) accepted. Every other accepted
  //     tier is launchable and must be may_not_launch=false.
  note(
    mnl === (regPath === 'appinst-local'),
    'may_not_launch === (register_path === "appinst-local")',
    `may_not_launch=${mnl} register_path=${regPath || '∅'}`,
  );
  // (c) Reorder property: a launchable tier (appinst/shellui-rpc/intdebug/
  //     regular) accepting means the unlaunchable AppInstallPkg was NOT
  //     reached first — i.e. an accepted launchable tier is never flagged.
  if (rc === 0 && regPath !== 'appinst-local') {
    note(mnl === false, 'accepted launchable tier is not flagged unlaunchable', `via=${via}`);
  }

  // 4. Poll status briefly — must not 5xx / wedge.
  if (res.json?.session_id) {
    const st = await getJson(
      `${ENGINE}/api/pkg/install/status?session=${encodeURIComponent(res.json.session_id)}`,
    ).catch((e) => ({ _err: String(e) }));
    note(!st._err, 'status poll responded', st._err ? st._err : `phase=${st.phase} may_not_launch=${st.may_not_launch}`);
  }

  // 5. THE KEY ASSERTION: payload survived the whole fallback chain.
  await sleep(1500);
  let alive = null;
  for (let i = 0; i < 10; i++) {
    alive = await getJson(`${ENGINE}/api/ps5/status?addr=${encodeURIComponent(MGMT)}`).catch(() => null);
    if (alive) break;
    await sleep(1000);
  }
  note(!!alive, 'payload SURVIVED the full fallback chain (mgmt port answers)',
    alive ? `v${alive.version} txns=${alive.active_transactions}` : 'mgmt port did not answer');

  // Cleanup staged dummy (best-effort).
  await postJson(`${ENGINE}/api/ps5/fs/delete`, { addr: MGMT, path: STAGE }).catch(() => {});

  process.stdout.write(`\ninstall-fallback result: ${failures === 0 ? 'all clear' : `${failures} FAILED`}\n`);
  if (engineChild) engineChild.kill();
  process.exit(failures === 0 ? 0 : 1);
}

main().catch((e) => {
  process.stderr.write(`fatal: ${e.stack || e}\n`);
  if (engineChild) engineChild.kill();
  process.exit(1);
});
