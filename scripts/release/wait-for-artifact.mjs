#!/usr/bin/env node
// Waits until an artifact of the current workflow run exists, so a job can do its setup
// (checkout, toolchains, npm ci) while the job that produces the artifact is still running,
// instead of `needs:`-waiting for it first. Used by publish.yml: the client and Android
// builds start with build-payload and wait here, just before they first need the ELFs.
//
//   node scripts/release/wait-for-artifact.mjs <artifact name> <producing job name>
//
// What ends the wait (decide() below):
//   - the artifact is listed: exit 0;
//   - the producing job finished without success (a bad tag, a broken payload, a runner GitHub
//     never handed over): exit 1 at once, nothing is compiled for it;
//   - the producing job succeeded but its artifact is still not listed 3 minutes later: exit 1;
//   - PS5UPLOAD_WAIT_MINUTES (default 45) have gone by: exit 1, saying whether the job was still
//     queued or running. 45 is build-payload's own job timeout, so a payload build that hangs is
//     ended by its job first and reported as the failure it is. The old 20 minutes was one clock
//     for queue and build together: v12.6.7's macOS x64 build failed with "did not appear in
//     time" because build-payload sat ~20 min in the runner queue and then built fine.
// A job that is queued or running is always waited for until that limit; an API error is not a
// verdict and is asked again.
// Needs GITHUB_TOKEN with actions: read; GITHUB_REPOSITORY and GITHUB_RUN_ID come from Actions.

export const DEFAULT_WAIT_MINUTES = 45;
/** How long a successful job's artifact may take to show up in the listing. */
export const LISTING_GRACE_MS = 3 * 60_000;

/**
 * One turn of the wait. `s`: {artifact, producer, artifactListed, job: {status, conclusion}|null,
 * now, startedAt, limitMs, successSeenAt: when the job was first seen completed+success (or null)}.
 * Returns {action: "ready"|"fail"|"wait", message, successSeenAt}.
 */
export function decide(s) {
  const waited = Math.round((s.now - s.startedAt) / 60_000);
  if (s.artifactListed) {
    return {
      action: "ready",
      message: `artifact ${s.artifact} is ready (waited ${waited} min)`,
      successSeenAt: s.successSeenAt ?? null,
    };
  }
  const job = s.job;
  let successSeenAt = s.successSeenAt ?? null;
  if (job && job.status === "completed") {
    if (job.conclusion !== "success") {
      return {
        action: "fail",
        message: `${s.producer} finished '${job.conclusion}' without producing ${s.artifact}`,
        successSeenAt,
      };
    }
    successSeenAt ??= s.now;
    if (s.now - successSeenAt > LISTING_GRACE_MS) {
      return {
        action: "fail",
        message: `${s.producer} succeeded but artifact ${s.artifact} is not listed ${Math.round(LISTING_GRACE_MS / 60_000)} min later`,
        successSeenAt,
      };
    }
  } else {
    successSeenAt = null;
  }
  const state = !job
    ? `${s.producer} is not in this run's job list yet`
    : job.status === "completed"
      ? `${s.producer} succeeded; waiting for its artifact to be listed`
      : job.status === "in_progress"
        ? `${s.producer} is running`
        : `${s.producer} is waiting for a runner (${job.status})`;
  if (s.now - s.startedAt > s.limitMs) {
    return {
      action: "fail",
      message: `artifact ${s.artifact} did not appear in time: after ${waited} min, ${state}`,
      successSeenAt,
    };
  }
  return { action: "wait", message: state, successSeenAt };
}

/** PS5UPLOAD_WAIT_MINUTES as milliseconds; anything that is not a positive number is the default. */
export function limitMs(raw) {
  const n = Number(raw);
  return (Number.isFinite(n) && n > 0 ? n : DEFAULT_WAIT_MINUTES) * 60_000;
}

if (process.argv[1]?.endsWith("wait-for-artifact.mjs")) {
  const [artifact, producer] = process.argv.slice(2);
  const { GITHUB_TOKEN, GITHUB_REPOSITORY, GITHUB_RUN_ID } = process.env;
  if (!artifact || !producer || !GITHUB_TOKEN || !GITHUB_REPOSITORY || !GITHUB_RUN_ID) {
    console.error(
      "usage: wait-for-artifact.mjs <artifact> <producing job> (GITHUB_TOKEN, GITHUB_REPOSITORY and GITHUB_RUN_ID set)",
    );
    process.exit(2);
  }
  const api = `https://api.github.com/repos/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}`;
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

  const startedAt = Date.now();
  const limit = limitMs(process.env.PS5UPLOAD_WAIT_MINUTES);
  let successSeenAt = null;
  let last = "";
  let artifactListed = false;
  let job = null;
  for (;;) {
    try {
      const list = await get(`${api}/artifacts?name=${encodeURIComponent(artifact)}`);
      artifactListed = (list.artifacts || []).some((a) => a.name === artifact && !a.expired);
      if (!artifactListed) {
        // filter=latest: on a re-run, the producing job of this attempt (or the one carried over).
        const jobs = await get(`${api}/jobs?filter=latest&per_page=100`);
        job = (jobs.jobs || []).find((j) => j.name === producer) ?? null;
      }
    } catch (err) {
      // A transient API error is not a verdict: the last known state stands until the limit.
      console.log(`(retrying: ${err.message})`);
    }
    const d = decide({
      artifact,
      producer,
      artifactListed,
      job,
      now: Date.now(),
      startedAt,
      limitMs: limit,
      successSeenAt,
    });
    successSeenAt = d.successSeenAt;
    if (d.action === "ready") {
      console.log(d.message);
      process.exit(0);
    }
    if (d.action === "fail") {
      console.error(`::error::${d.message}`);
      process.exit(1);
    }
    if (d.message !== last) {
      console.log(`${d.message} (waited ${Math.round((Date.now() - startedAt) / 60_000)} min)`);
      last = d.message;
    }
    await new Promise((r) => setTimeout(r, 10_000));
  }
}
