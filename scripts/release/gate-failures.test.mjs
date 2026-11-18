import assert from "node:assert/strict";
import test from "node:test";
import { classifyJob, classifyRun, fetchJobs, playwrightFlakyOnly } from "./gate-failures.mjs";

// Job lists shaped like the real ones (names, conclusions, steps and annotations as the API gave
// them for the runs named in each test; logs cut down to the lines that matter).
const ok = (name) => ({ name, conclusion: "success", runner_name: "GitHub Actions 1", steps: [{ name: "Run", conclusion: "success" }] });
const skipped = (name) => ({ name, conclusion: "skipped", runner_name: "", steps: [] });
const gate = {
  name: "PR gate",
  conclusion: "failure",
  runner_name: "GitHub Actions 1000061523",
  steps: [{ name: "Every job this change needed succeeded; the others were skipped on purpose", conclusion: "failure" }],
  annotations: ["Process completed with exit code 1.", "rust-desktop: failure, and this change needed it (lane desktop)"],
  log: "",
};
const exit1 = "Process completed with exit code 1.";

const aptFailure = {
  name: "rust desktop (ubuntu-24.04)",
  conclusion: "failure",
  runner_name: "GitHub Actions 1000061499",
  steps: [
    { name: "Set up job", conclusion: "success" },
    { name: "Install Tauri Linux deps", conclusion: "failure" },
    { name: "Build", conclusion: "skipped" },
  ],
  annotations: [exit1, "apt could not download after 3 attempts: libgtk-3-dev libwebkit2gtk-4.1-dev patchelf build-essential clang"],
  log: "",
};
const pw = (summary) =>
  [
    "client browser tests (3/3)\tUNKNOWN STEP\t2026-11-18T07:14:37.8774637Z [109/108] [app-csp] › e2e/remote-play.spec.ts:1037:1 › Remote Play: the game's sound plays",
    ...summary.map((l) => `client browser tests (3/3)\tUNKNOWN STEP\t2026-11-18T07:14:42.0240514Z ${l}`),
    "client browser tests (3/3)\tUNKNOWN STEP\t2026-11-18T07:14:42.1190397Z ##[error]Process completed with exit code 1.",
  ].join("\n");
const browserJob = (summary) => ({
  name: "client browser tests (3/3)",
  conclusion: "failure",
  runner_name: "GitHub Actions 1000061955",
  steps: [{ name: "Browser workflow smoke test", conclusion: "failure" }],
  annotations: [exit1],
  log: pw(summary),
});
const flakyOnly = browserJob(["  1 flaky", "    [chromium-desktop] › e2e/settings-tabs.spec.ts:40:3 › App and This PS5 tabs (1280px) ", "  107 passed (5.1m)"]);
const starved = {
  name: "build-client-macos-arm64",
  conclusion: "cancelled",
  runner_name: "",
  steps: [],
  annotations: [
    "The job was not acquired by Runner of type hosted even after multiple attempts",
    "Due to capacity constraints, jobs targeting macOS arm64 runners may experience longer queue times.",
  ],
  log: "",
};
const realFailure = {
  name: "security scan",
  conclusion: "failure",
  runner_name: "GitHub Actions 1000061171",
  steps: [{ name: "Scan", conclusion: "failure" }],
  annotations: [exit1],
  log: "security scan\tUNKNOWN STEP\t2026-11-17T18:19:58.0Z error: 1 finding\nsecurity scan\tUNKNOWN STEP\t2026-11-17T18:19:58.1Z ##[error]Process completed with exit code 1.",
};

test("apt giving up on the mirrors is infra (ci run 44221552127, rust desktop)", () => {
  const j = classifyJob(aptFailure);
  assert.equal(j.kind, "infra");
  assert.match(j.reason, /apt/);
  const run = classifyRun([ok("version sync"), aptFailure, gate, skipped("docs")]);
  assert.deepEqual([run.verdict, run.rerun], ["infra", true]);
  assert.deepEqual(run.jobs.map((x) => x.kind), ["infra", "aggregate"]);
});

test("a job GitHub never gave a runner is infra (publish run 44312908828, macOS arm64)", () => {
  assert.equal(classifyJob(starved).kind, "infra");
  // A failed job with no runner and no steps never started either.
  assert.equal(classifyJob({ ...starved, conclusion: "failure", annotations: [] }).kind, "infra");
});

test("a queued job cancelled by a newer push (no runner, no steps) is not infra", () => {
  const superseded = {
    ...starved,
    name: "rust desktop (macos-15)",
    annotations: ["Canceling since a higher priority waiting request for ci-refs/pull/861/merge exists"],
  };
  assert.equal(classifyJob(superseded).kind, "cancelled");
  assert.equal(classifyJob({ ...superseded, annotations: [] }).kind, "cancelled");
  // A whole run cancelled that way is not re-run.
  assert.deepEqual(classifyRun([superseded, { ...superseded, name: "payload" }]).rerun, false);
});

test("the artifact wait running out is infra (publish run 44253579748, macOS x64)", () => {
  const j = classifyJob({
    name: "build-client-macos-x64",
    conclusion: "failure",
    runner_name: "GitHub Actions 1000061896",
    steps: [{ name: "Wait for the payload ELF", conclusion: "failure" }],
    annotations: [exit1, "artifact ps5upload-payload did not appear in time"],
    log: "",
  });
  assert.equal(j.kind, "infra");
  assert.match(j.reason, /artifact wait/);
});

test("a CoreSimulator crash in an iOS build is infra, from the log alone", () => {
  const j = classifyJob({
    name: "build-ios",
    conclusion: "failure",
    runner_name: "GitHub Actions 7",
    steps: [{ name: "Build unsigned IPA (arm64)", conclusion: "failure" }],
    annotations: [exit1],
    log: "build-ios\tBuild\t2026-11-18T05:20:00.0Z error: Command CompileStoryboard failed with a nonzero exit code\nbuild-ios\tBuild\t2026-11-18T05:20:00.1Z   CoreSimulator.framework backtrace",
  });
  assert.equal(j.kind, "infra");
});

test("Playwright flaky-only is flaky; one real failure beside it is real", () => {
  assert.equal(classifyJob(flakyOnly).kind, "flaky");
  const failed = browserJob(["  1 failed", "    [chromium-desktop] › e2e/library.spec.ts:12:1 › shows the library ", "  1 flaky", "  106 passed (5.3m)"]);
  assert.equal(classifyJob(failed).kind, "real");
  assert.equal(classifyJob(browserJob(["  2 failed", "  106 passed (5.3m)"])).kind, "real");
  assert.equal(classifyJob(browserJob(["  1 flaky", "  3 did not run", "  100 passed (5.3m)"])).kind, "real");
  // "0 flaky" or a log with no summary at all is not a flake.
  assert.equal(playwrightFlakyOnly("  0 flaky\n  108 passed (5m)"), false);
  assert.equal(playwrightFlakyOnly("error: something broke"), false);
});

test("main's run 44221552127: a flaky browser shard and an apt failure together re-run once", () => {
  const run = classifyRun([flakyOnly, aptFailure, gate, ok("payload")]);
  assert.deepEqual([run.verdict, run.rerun], ["flaky", true]);
});

test("a real failure is real, and one real job makes the whole run real (PR run 44146086959)", () => {
  assert.equal(classifyJob(realFailure).kind, "real");
  assert.match(classifyJob(realFailure).reason, /failed step: Scan/);
  for (const jobs of [[realFailure, gate], [realFailure, aptFailure, starved, flakyOnly, gate]]) {
    const run = classifyRun(jobs);
    assert.deepEqual([run.verdict, run.rerun], ["real", false]);
  }
});

test("failed tests and compile errors are never excused", () => {
  const cargo = {
    name: "rust engine tests (engine)",
    conclusion: "failure",
    runner_name: "GitHub Actions 3",
    steps: [{ name: "cargo nextest", conclusion: "failure" }],
    annotations: [exit1],
    log: "x\ty\t2026-11-18T00:00:00.0Z      Summary [ 201.1s] 1412 tests run: 1411 passed, 1 failed, 3 skipped\nx\ty\t2026-11-18T00:00:00.1Z error: test run failed",
  };
  assert.equal(classifyJob(cargo).kind, "real");
  const compile = { ...cargo, log: "x\ty\t2026-11-18T00:00:00.0Z error[E0308]: mismatched types\nx\ty\t2026-11-18T00:00:00.1Z error: could not compile `ps5upload-engine`" };
  assert.equal(classifyJob(compile).kind, "real");
});

test("a job that ran past its timeout is real, even with no failed step", () => {
  const j = classifyJob({
    name: "client browser tests (1/3)",
    conclusion: "cancelled",
    runner_name: "GitHub Actions 9",
    steps: [{ name: "Browser workflow smoke test", conclusion: "cancelled" }],
    annotations: ["The job running on runner GitHub Actions 9 has exceeded the maximum execution time of 30 minutes.", "The operation was canceled."],
    log: "",
  });
  assert.deepEqual([j.kind, j.reason], ["real", "ran past its timeout"]);
});

test("cancelled jobs and the gate alone explain nothing: real, no re-run", () => {
  const cancelled = {
    name: "rust engine tests (rest)",
    conclusion: "cancelled",
    runner_name: "GitHub Actions 4",
    steps: [{ name: "cargo nextest", conclusion: "cancelled" }],
    annotations: ["The operation was canceled."],
    log: "",
  };
  assert.equal(classifyJob(cancelled).kind, "cancelled");
  assert.deepEqual(classifyRun([cancelled, gate]).verdict, "real");
  assert.deepEqual(classifyRun([gate]).verdict, "real");
  // Beside an infra failure, a cancelled sibling does not block the re-run.
  assert.deepEqual(classifyRun([cancelled, starved, gate]).verdict, "infra");
});

test("an attempt with no failed job has no verdict to act on", () => {
  assert.deepEqual(classifyRun([ok("a"), skipped("b")]), { verdict: "none", rerun: false, jobs: [] });
});

test("an unreadable job (no annotations, no log) is real", () => {
  const j = classifyJob({ name: "payload", conclusion: "failure", runner_name: "GitHub Actions 2", steps: [{ name: "make payload", conclusion: "failure" }] });
  assert.equal(j.kind, "real");
});

test("fetchJobs reads annotations and the failed log for failed jobs only", () => {
  const calls = [];
  const fake = (args) => {
    calls.push(args.join(" "));
    if (args[0] === "api" && args[1].includes("/attempts/2/jobs")) {
      return JSON.stringify({
        jobs: [
          { id: 1, ...ok("version sync") },
          { id: 2, ...aptFailure, annotations: undefined, log: undefined },
          { id: 3, ...gate, annotations: undefined, log: undefined },
          { id: 4, ...starved, annotations: undefined, log: undefined },
        ],
      });
    }
    if (args[0] === "api" && args[1].includes("/check-runs/2/")) return JSON.stringify([{ message: "apt could not download after 3 attempts: x" }]);
    if (args[0] === "api" && args[1].includes("/check-runs/4/")) return JSON.stringify([{ message: "The job was not acquired by Runner of type hosted even after multiple attempts" }]);
    if (args[0] === "run") throw new Error("log not found");
    throw new Error(`unexpected gh ${args.join(" ")}`);
  };
  const run = classifyRun(fetchJobs("o/r", "99", "2", fake));
  assert.deepEqual(run.jobs.map((j) => [j.name, j.kind]), [
    ["rust desktop (ubuntu-24.04)", "infra"],
    ["PR gate", "aggregate"],
    ["build-client-macos-arm64", "infra"],
  ]);
  // No log for the job with no failed step, none for the gate, none for the passing job.
  assert.deepEqual(calls.filter((c) => c.startsWith("run view")).length, 1);
  assert.ok(!calls.some((c) => c.includes("/check-runs/1/") || c.includes("/check-runs/3/")));
});
