#!/usr/bin/env node
// Why did this workflow run fail: something outside the change (worth one re-run), or the change?
//
//   node scripts/release/gate-failures.mjs <run id> [--attempt N] [--repo owner/name]
//
// Prints JSON: {run, attempt, verdict, rerun, jobs: [{name, conclusion, kind, reason}]}.
// Read-only: it never re-runs anything. scripts/release/merge-when-green.sh and
// .github/workflows/ci-runner-retry.yml decide what to do with the verdict.
//
// Each failed or cancelled job gets a kind:
//   infra      nothing the change did:
//                - GitHub never gave the job a runner ("not acquired by Runner", or a failed
//                  job with no runner and no steps);
//                - the runner went away under it (shutdown signal, lost communication);
//                - scripts/ci-apt.sh gave up on the mirrors ("apt could not download");
//                - scripts/release/wait-for-artifact.mjs gave up ("did not appear in time");
//                - the macOS simulator service crashed under an iOS build (CoreSimulator).
//   flaky      Playwright's own verdict: every test passed, some only on their retry ("N flaky"
//              and no "N failed"; CI runs with failOnFlakyTests so these stay visible).
//   aggregate  the `PR gate` job: it fails because another job did; says nothing by itself.
//   cancelled  cancelled after it had started, with no sign of the above: a sibling's failure,
//              a newer push, or a person. Says nothing by itself.
//   real       everything else, and anything this cannot read: a failed test, a compile error,
//              a lint, a job that ran past its timeout. Never re-run automatically.
//
// The run's verdict: `real` if any job is real, or if nothing but aggregate/cancelled jobs
// failed (nothing explains the failure, so nothing excuses it); else `flaky` if any job is flaky;
// else `infra`. `rerun` is true for infra and flaky. `none`: the attempt has no failed job.
// Exit status: 0 with a verdict printed, 2 on bad usage or a failed lookup (not a verdict).

import { execFileSync } from "node:child_process";

export const AGGREGATE_JOBS = new Set(["PR gate"]);

const INFRA_PATTERNS = [
  [/not acquired by Runner/i, "GitHub never gave the job a runner"],
  [/runner has received a shutdown signal|lost communication with the server|runner.*was (?:lost|evicted)/i, "the runner went away"],
  [/apt could not download after \d+ attempts/i, "apt mirrors (ci-apt.sh gave up)"],
  [/artifact \S+ did not appear in time/i, "the artifact wait ran out (wait-for-artifact.mjs)"],
  [/CoreSimulator|Command Compile(?:Storyboard|AssetCatalog[A-Za-z]*) failed/, "the macOS simulator service crashed"],
];
const TIMEOUT_PATTERN = /exceeded the maximum execution time|has timed out after \d+ minutes/i;

/** Playwright's summary: "N flaky" present and no "N failed" / "N interrupted" / "did not run". */
export function playwrightFlakyOnly(log) {
  const line = (word) => new RegExp(`^\\s*(\\d+) ${word}\\s*$`, "m").exec(log);
  const flaky = line("flaky");
  if (!flaky || Number(flaky[1]) === 0) return false;
  if (line("failed") || line("interrupted") || line("did not run")) return false;
  return /^\s*\d+ passed\b/m.test(log);
}

/** Log lines as `gh run view --log-failed` prints them, without the "job\tstep\ttimestamp " prefix. */
export function stripLogPrefix(log) {
  return log
    .split("\n")
    .map((l) => l.replace(/^[^\t]*\t[^\t]*\t/, "").replace(/^﻿?\d{4}-\d\d-\d\dT[\d:.]+Z ?/, ""))
    .join("\n");
}

/**
 * job: {name, conclusion, runner_name, steps: [{name, conclusion}], annotations: [message],
 *       log: the failed steps' log text (may be "")}. Returns {name, conclusion, kind, reason}.
 */
export function classifyJob(job) {
  const out = (kind, reason) => ({ name: job.name, conclusion: job.conclusion, kind, reason });
  if (AGGREGATE_JOBS.has(job.name)) return out("aggregate", "fails when another job does");
  const notes = (job.annotations ?? []).join("\n");
  const log = stripLogPrefix(job.log ?? "");
  const steps = job.steps ?? [];
  // A job that ran past its timeout is the change's until someone shows otherwise.
  if (TIMEOUT_PATTERN.test(notes)) return out("real", "ran past its timeout");
  for (const [pattern, reason] of INFRA_PATTERNS) {
    if (pattern.test(notes)) return out("infra", reason);
  }
  // A FAILED job with no runner and no steps never started. A CANCELLED one like that is usually
  // a queued job a newer push cancelled (the run's concurrency group), which must not be re-run:
  // for a cancelled job only GitHub's own "not acquired by Runner" note above counts.
  const ranNothing = steps.length === 0 || steps.every((s) => s.conclusion === "skipped" || s.conclusion == null);
  if (ranNothing && !job.runner_name && job.conclusion !== "cancelled") {
    return out("infra", "no runner and no steps: the job never started");
  }
  for (const [pattern, reason] of INFRA_PATTERNS) {
    if (pattern.test(log)) return out("infra", reason);
  }
  if (job.conclusion === "failure" && playwrightFlakyOnly(log)) {
    return out("flaky", "Playwright: every test passed, some only on retry");
  }
  if (job.conclusion === "cancelled") return out("cancelled", "cancelled after it started");
  const failed = steps.filter((s) => s.conclusion === "failure").map((s) => s.name);
  return out("real", failed.length ? `failed step: ${failed.join(", ")}` : `${job.conclusion}, cause not recognised`);
}

const FAILED = new Set(["failure", "cancelled", "timed_out", "startup_failure", "action_required"]);

/** jobs: every job of the attempt. Returns {verdict, rerun, jobs: classified failed jobs}. */
export function classifyRun(jobs) {
  const classified = jobs.filter((j) => FAILED.has(j.conclusion)).map(classifyJob);
  const kinds = new Set(classified.map((j) => j.kind));
  let verdict;
  if (classified.length === 0) verdict = "none";
  else if (kinds.has("real")) verdict = "real";
  else if (kinds.has("flaky")) verdict = "flaky";
  else if (kinds.has("infra")) verdict = "infra";
  else verdict = "real"; // only the gate and cancelled jobs: nothing explains it
  return { verdict, rerun: verdict === "infra" || verdict === "flaky", jobs: classified };
}

// ─── The real lookups (gh) ───

function gh(args) {
  return execFileSync("gh", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], maxBuffer: 256 * 1024 * 1024 });
}

export function fetchJobs(repo, run, attempt, ghFn = gh) {
  const jobs = [];
  for (let page = 1; page <= 10; page++) {
    const body = JSON.parse(ghFn(["api", `repos/${repo}/actions/runs/${run}/attempts/${attempt}/jobs?per_page=100&page=${page}`]));
    jobs.push(...(body.jobs ?? []));
    if ((body.jobs ?? []).length < 100) break;
  }
  for (const job of jobs) {
    if (!FAILED.has(job.conclusion) || AGGREGATE_JOBS.has(job.name)) continue;
    // A job's id is its check run's id. Annotations carry every ::error:: line and GitHub's own
    // notes ("not acquired by Runner"); a lookup that fails must not turn into "infra".
    job.annotations = JSON.parse(ghFn(["api", `repos/${repo}/check-runs/${job.id}/annotations?per_page=100`])).map(
      (a) => a.message ?? "",
    );
    job.log = "";
    if ((job.steps ?? []).some((s) => s.conclusion === "failure")) {
      try {
        // Only the tail: the summary lines this reads are at the end of a failed step.
        job.log = ghFn(["run", "view", "--repo", repo, "--job", String(job.id), "--log-failed"]).split("\n").slice(-400).join("\n");
      } catch {
        // Logs can be gone or still being stored. Without them the job is `real` unless its
        // annotations say otherwise, which is the safe way round.
      }
    }
  }
  return jobs;
}

if (process.argv[1]?.endsWith("gate-failures.mjs")) {
  const args = process.argv.slice(2);
  const opt = (name) => {
    const i = args.indexOf(name);
    return i >= 0 ? args[i + 1] : undefined;
  };
  const run = args.find((a, i) => /^\d+$/.test(a) && !["--attempt", "--repo"].includes(args[i - 1]));
  if (!run) {
    process.stderr.write("usage: gate-failures.mjs <run id> [--attempt N] [--repo owner/name]\n");
    process.exit(2);
  }
  try {
    const repo = opt("--repo") ?? process.env.GITHUB_REPOSITORY ?? JSON.parse(gh(["repo", "view", "--json", "nameWithOwner"])).nameWithOwner;
    const attempt = opt("--attempt") ?? String(JSON.parse(gh(["api", `repos/${repo}/actions/runs/${run}`])).run_attempt);
    if (!/^\d+$/.test(attempt)) throw new Error(`bad attempt '${attempt}'`);
    const result = classifyRun(fetchJobs(repo, run, attempt));
    process.stdout.write(`${JSON.stringify({ run: Number(run), attempt: Number(attempt), ...result })}\n`);
  } catch (err) {
    process.stderr.write(`gate-failures: lookup failed: ${String(err.stderr || err.message || err).trim().split("\n")[0]}\n`);
    process.exit(2);
  }
}
