#!/usr/bin/env node
// Virus-scans this workflow run's build artifacts as each one is uploaded, so the scan is done
// by the time the last build finishes instead of starting then. publish.yml's virus-scan job runs
// it beside the builds (ClamAV and today's signatures already installed); create-release then
// scans only what this did not cover.
//
//   node scripts/release/scan-artifacts.mjs <out dir> [extra file ...]
//
// Polls the run's artifacts named ps5upload-*, downloads and unzips each new one, and scans its
// files with clamscan-parallel.sh (the extra files, e.g. LICENSE, go into the first round). Ends
// once every `build-*` job of the run has finished and its artifacts are scanned, or at
// PS5UPLOAD_SCAN_MINUTES (default 100). Writes <out dir>/scanned.sha256: the SHA-256 of every
// file that scanned clean, one per line. create-release scans any release file whose hash is not
// in it, so a file this missed (a re-run's rebuild, a timeout) is still scanned before it ships.
// Exits 1 on a hit or a scan error, and writes nothing then.
// Needs GITHUB_TOKEN with actions: read; GITHUB_REPOSITORY and GITHUB_RUN_ID come from Actions.

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream, mkdirSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

/** Every file under `dir`, recursively. */
export function listFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...listFiles(p));
    else if (entry.isFile()) out.push(p);
  }
  return out.sort();
}

/** The artifacts to scan that have not been: build outputs only (ps5upload-*), live ones. */
export function newArtifacts(artifacts, seen) {
  return artifacts.filter((a) => a.name.startsWith("ps5upload-") && !a.expired && !seen.has(a.id));
}

/** True once the run has build jobs and every one of them has finished. */
export function buildsDone(jobs) {
  const builds = jobs.filter((j) => j.name.startsWith("build-"));
  return builds.length > 0 && builds.every((j) => j.status === "completed");
}

function sha256(file) {
  return new Promise((resolve, reject) => {
    const h = createHash("sha256");
    createReadStream(file)
      .on("data", (d) => h.update(d))
      .on("end", () => resolve(h.digest("hex")))
      .on("error", reject);
  });
}

async function main() {
  const [outDir, ...extra] = process.argv.slice(2);
  const { GITHUB_TOKEN, GITHUB_REPOSITORY, GITHUB_RUN_ID, RUNNER_TEMP } = process.env;
  if (!outDir || !GITHUB_TOKEN || !GITHUB_REPOSITORY || !GITHUB_RUN_ID) {
    console.error("usage: scan-artifacts.mjs <out dir> [extra file ...] (GITHUB_TOKEN, GITHUB_REPOSITORY, GITHUB_RUN_ID set)");
    process.exit(2);
  }
  const repoApi = `${process.env.GITHUB_API_URL || "https://api.github.com"}/repos/${GITHUB_REPOSITORY}`;
  const runApi = `${repoApi}/actions/runs/${GITHUB_RUN_ID}`;
  const headers = {
    Accept: "application/vnd.github+json",
    Authorization: `Bearer ${GITHUB_TOKEN}`,
    "X-GitHub-Api-Version": "2022-11-28",
  };
  const get = async (url) => {
    const res = await fetch(url, { headers, signal: AbortSignal.timeout(30_000) });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText} for ${url}`);
    return res.json();
  };
  const scanner = join(dirname(fileURLToPath(import.meta.url)), "clamscan-parallel.sh");
  const work = join(RUNNER_TEMP || "/tmp", "scan-artifacts");
  mkdirSync(work, { recursive: true });
  mkdirSync(outDir, { recursive: true });
  const deadline = Date.now() + Number(process.env.PS5UPLOAD_SCAN_MINUTES || 100) * 60_000;

  const seen = new Set();
  const clean = new Set();
  let pending = [...extra];
  // Failed rounds after the builds finished: a download that keeps failing is left to
  // create-release after a few, rather than holding the release.
  let finishedSeen = false;
  let failedAfterFinish = 0;

  for (;;) {
    // Builds first, artifacts second: a job that is finished has uploaded everything it will.
    let finished = false;
    let ok = true;
    try {
      finished = buildsDone((await get(`${runApi}/jobs?filter=latest&per_page=100`)).jobs || []);
      const list = await get(`${runApi}/artifacts?per_page=100`);
      for (const a of newArtifacts(list.artifacts || [], seen)) {
        const zip = join(work, `${a.id}.zip`);
        const res = await fetch(`${repoApi}/actions/artifacts/${a.id}/zip`, {
          headers,
          signal: AbortSignal.timeout(15 * 60_000),
        });
        if (!res.ok) throw new Error(`${res.status} ${res.statusText} downloading ${a.name}`);
        await pipeline(Readable.fromWeb(res.body), createWriteStream(zip));
        const dir = join(work, String(a.id));
        const unzip = spawnSync("unzip", ["-q", "-o", zip, "-d", dir], { stdio: "inherit" });
        if (unzip.status !== 0) throw new Error(`unzip of ${a.name} failed`);
        const files = listFiles(dir);
        console.log(`${a.name}: ${files.length} file(s)`);
        pending.push(...files);
        seen.add(a.id);
      }
    } catch (err) {
      // A transient API or download error is not a verdict: try again next round. Whatever is
      // never scanned here, create-release scans.
      console.log(`(retrying: ${err.message})`);
      ok = false;
    }
    if (finished) finishedSeen = true;

    if (pending.length > 0) {
      const scan = spawnSync("bash", [scanner], {
        input: pending.map((f) => `${f}\0`).join(""),
        stdio: ["pipe", "inherit", "inherit"],
      });
      if (scan.status !== 0) {
        console.error("::error::ClamAV reported a hit or an error (above); nothing is marked scanned");
        process.exit(1);
      }
      for (const f of pending) {
        if (statSync(f).isFile()) clean.add(await sha256(f));
      }
      pending = [];
    }

    // The jobs were read before the artifacts, so a clean round that found every build finished
    // saw every upload: nothing more will come.
    if (finished && ok) break;
    if (finishedSeen && !ok && ++failedAfterFinish >= 4) {
      console.log("::warning::scan-artifacts gave up on an artifact; create-release scans what it did not cover");
      break;
    }
    if (Date.now() > deadline) {
      console.log("::warning::scan-artifacts ran out of time; create-release scans what it did not cover");
      break;
    }
    await new Promise((r) => setTimeout(r, Number(process.env.PS5UPLOAD_SCAN_POLL_SECONDS || 15) * 1000));
  }

  writeFileSync(join(outDir, "scanned.sha256"), [...clean].sort().map((h) => `${h}\n`).join(""));
  console.log(`scanned clean: ${clean.size} file(s) from ${seen.size} artifact(s)`);
}

if (process.argv[1]?.endsWith("scan-artifacts.mjs")) {
  await main();
}
