#!/usr/bin/env node
// The `PR gate` verdict for engine-ci.yml: one check that is green only when every job the change
// needed succeeded, and every job it did not need was skipped on purpose.
//
//   NEEDS='${{ toJSON(needs) }}' node .github/scripts/pr-gate.mjs
//
// A skipped job passes only when its lane (from the `changes` job's outputs) was off. A job whose
// lane was on and that did not succeed, for whatever reason (failed, cancelled, or skipped because
// something it needs failed), fails the gate. So does a job this file does not know: add it to
// JOB_LANES with its lane, or `always` when it runs on every change.

export const JOB_LANES = {
  changes: "always",
  "version-sync": "always",
  "security-scan": "always",
  "repo-scripts": "always",
  "rust-engine": "engine",
  "rust-engine-tests": "engine",
  "rust-golive": "engine",
  "rust-engine-targets": "engine",
  ava1: "engine",
  "rust-hub": "hub_only",
  "rust-ctest": "ctest",
  "ava1-ctest-sanitize": "ctest",
  payload: "payload",
  "payload-selftests": "payload",
  "rust-desktop": "desktop",
  android: "android",
  "client-frontend": "client",
};

/**
 * `needs`: the workflow's `needs` context ({job: {result, outputs}}). Returns the problems found;
 * an empty list is a pass.
 */
export function gateProblems(needs) {
  const problems = [];
  const changes = needs.changes;
  if (!changes || changes.result !== "success") {
    problems.push(`changes: ${changes ? changes.result : "missing"} (cannot tell which jobs this change needed)`);
    return problems;
  }
  const lanes = changes.outputs ?? {};
  for (const [job, { result }] of Object.entries(needs)) {
    const lane = JOB_LANES[job];
    if (!lane) {
      problems.push(`${job}: not in pr-gate.mjs's JOB_LANES; say which lane runs it`);
      continue;
    }
    const needed = lane === "always" || lanes[lane] === "true";
    if (lane !== "always" && lanes[lane] !== "true" && lanes[lane] !== "false") {
      problems.push(`${job}: lane '${lane}' has no true/false output from changes (got '${lanes[lane]}')`);
      continue;
    }
    if (needed && result !== "success") {
      problems.push(`${job}: ${result}, and this change needed it (lane ${lane})`);
    } else if (!needed && result !== "skipped" && result !== "success") {
      problems.push(`${job}: ${result} (lane ${lane} was off, so it should have been skipped)`);
    }
  }
  for (const job of Object.keys(JOB_LANES)) {
    if (!(job in needs)) problems.push(`${job}: missing from pr-gate's needs`);
  }
  return problems;
}

if (process.argv[1]?.endsWith("pr-gate.mjs")) {
  const needs = JSON.parse(process.env.NEEDS ?? "{}");
  for (const [job, { result }] of Object.entries(needs)) {
    process.stdout.write(`${result.padEnd(9)} ${job} (lane ${JOB_LANES[job] ?? "?"})\n`);
  }
  const problems = gateProblems(needs);
  if (problems.length) {
    for (const p of problems) process.stdout.write(`::error::${p}\n`);
    process.exit(1);
  }
  process.stdout.write("every job this change needed succeeded; the rest were skipped on purpose\n");
}
