// scripts/release/merge-when-green.sh against a fake `gh` and a fake gate wait: it merges only
// on a pass, re-runs once and only for failures a re-run can fix, and cannot be fooled by a pipe.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import nodeTest from "node:test";

// The fake `gh` is a bash script on PATH. On Windows, node (gate-failures.mjs) starts the real
// gh.exe instead of it, so these tests would ask GitHub. The script is run from macOS or Linux
// (a release is cut there); on Windows the tests are skipped, like the other repo script tests
// that need Unix tools.
const test =
  process.platform === "win32"
    ? (name, fn) => nodeTest(name, { skip: "needs a POSIX shell's fake gh" }, fn)
    : nodeTest;

const script = path.join(import.meta.dirname, "merge-when-green.sh");
const SHA = "a".repeat(40);

// A `gh` that answers from files in $FAKE_STATE and writes every call to calls.log.
const FAKE_GH = `#!/usr/bin/env bash
S="$FAKE_STATE"
echo "gh $*" >> "$S/calls.log"
case "$1 $2" in
  "repo view") echo "o/r" ;;
  "pr view") cat "$S/pr.json" ;;
  "pr merge") exit "$(cat "$S/merge-rc" 2>/dev/null || echo 0)" ;;
  "run view") cat "$S/log.txt" 2>/dev/null || exit 1 ;;
  "run rerun")
    mode="$(cat "$S/rerun-mode" 2>/dev/null || echo ok)"
    if [ "$mode" = fail ]; then echo "run 77 cannot be rerun; its workflow file may be broken" >&2; exit 1; fi
    # The run moves to its next attempt and finishes; that attempt's jobs are jobs-after.json.
    n="$(jq .run_attempt "$S/run.json")"
    jq -n --argjson n "$((n + 1))" '{id: 77, run_attempt: $n, status: "completed"}' > "$S/run.json"
    cp "$S/jobs-after.json" "$S/jobs.json" 2>/dev/null
    if [ "$mode" = already ]; then echo "run 77 cannot be rerun; This workflow is already running" >&2; exit 1; fi
    ;;
  api\\ *)
    case "$2" in
      */workflows/engine-ci.yml/runs*) jq '{workflow_runs: [. + {created_at: "2026-11-18T00:00:00Z"}]}' "$S/run.json" ;;
      */attempts/*/jobs*) cat "$S/jobs.json" ;;
      */check-runs/*/annotations*) jq --arg id "$(echo "$2" | sed -E 's#.*/check-runs/([0-9]+)/.*#\\1#')" '.[$id] // []' "$S/annotations.json" ;;
      */actions/runs/77) cat "$S/run.json" ;;
      *) echo "fake gh: unexpected api $2" >&2; exit 1 ;;
    esac
    ;;
  *) echo "fake gh: unexpected $*" >&2; exit 1 ;;
esac
`;
// The gate wait: exits with the next code in gate-codes (the last one repeats).
const FAKE_WAIT = `#!/usr/bin/env bash
S="$FAKE_STATE"
echo "wait-pr-gate $*" >> "$S/calls.log"
code="$(head -n 1 "$S/gate-codes")"
if [ "$(wc -l < "$S/gate-codes")" -gt 1 ]; then tail -n +2 "$S/gate-codes" > "$S/gate-codes.new"; cat "$S/gate-codes.new" > "$S/gate-codes"; fi
echo "PR gate: exit $code"
exit "$code"
`;

const gateJob = { id: 9, name: "PR gate", conclusion: "failure", runner_name: "r", steps: [{ name: "gate", conclusion: "failure" }] };
const aptJob = { id: 2, name: "rust desktop (ubuntu-24.04)", conclusion: "failure", runner_name: "r", steps: [{ name: "Install Tauri Linux deps", conclusion: "failure" }] };
const realJob = { id: 3, name: "security scan", conclusion: "failure", runner_name: "r", steps: [{ name: "Scan", conclusion: "failure" }] };
const browserJob = { id: 4, name: "client browser tests (3/3)", conclusion: "failure", runner_name: "r", steps: [{ name: "Browser workflow smoke test", conclusion: "failure" }] };
const ANNOTATIONS = { 2: [{ message: "apt could not download after 3 attempts: clang" }], 3: [{ message: "Process completed with exit code 1." }] };

// Each run's folder is removed when the file's tests end, pass or fail (PS5UPLOAD_KEEP_TEST_TMP=1
// keeps them to look at).
const runDirs = [];
nodeTest.after(() => {
  if (process.env.PS5UPLOAD_KEEP_TEST_TMP) return;
  for (const dir of runDirs) fs.rmSync(dir, { recursive: true, force: true });
});

function run({ gate, jobs = [], jobsAfter, pr = { state: "OPEN", isDraft: false, headRefOid: SHA }, rerunMode, mergeRc, log, args = [], shell }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "merge-when-green-"));
  runDirs.push(dir);
  const bin = path.join(dir, "bin");
  fs.mkdirSync(bin);
  fs.writeFileSync(path.join(bin, "gh"), FAKE_GH, { mode: 0o755 });
  fs.writeFileSync(path.join(dir, "wait"), FAKE_WAIT, { mode: 0o755 });
  fs.writeFileSync(path.join(dir, "calls.log"), "");
  fs.writeFileSync(path.join(dir, "gate-codes"), `${gate.join("\n")}\n`);
  fs.writeFileSync(path.join(dir, "pr.json"), JSON.stringify(pr));
  fs.writeFileSync(path.join(dir, "run.json"), JSON.stringify({ id: 77, run_attempt: 1, status: "completed" }));
  fs.writeFileSync(path.join(dir, "jobs.json"), JSON.stringify({ jobs }));
  fs.writeFileSync(path.join(dir, "jobs-after.json"), JSON.stringify({ jobs: jobsAfter ?? jobs }));
  fs.writeFileSync(path.join(dir, "annotations.json"), JSON.stringify(ANNOTATIONS));
  if (rerunMode) fs.writeFileSync(path.join(dir, "rerun-mode"), rerunMode);
  if (mergeRc !== undefined) fs.writeFileSync(path.join(dir, "merge-rc"), String(mergeRc));
  if (log) fs.writeFileSync(path.join(dir, "log.txt"), log);
  const env = {
    ...process.env,
    PATH: `${bin}${path.delimiter}${process.env.PATH}`,
    FAKE_STATE: dir,
    MERGE_WHEN_GREEN_WAIT: path.join(dir, "wait"),
    MERGE_WHEN_GREEN_POLL: "0",
  };
  delete env.GITHUB_REPOSITORY;
  const res = shell
    ? spawnSync("bash", ["-c", shell.replace("@SCRIPT@", script)], { encoding: "utf8", env })
    : spawnSync("bash", [script, "123", ...args], { encoding: "utf8", env });
  const calls = fs.readFileSync(path.join(dir, "calls.log"), "utf8").trim().split("\n");
  return {
    status: res.status,
    out: res.stdout + res.stderr,
    calls,
    merges: calls.filter((c) => c.startsWith("gh pr merge")),
    reruns: calls.filter((c) => c.startsWith("gh run rerun")),
    waits: calls.filter((c) => c.startsWith("wait-pr-gate")),
  };
}
const MERGE = `gh pr merge 123 --squash --admin --match-head-commit ${SHA}`;

test("gate passes: merged, squash + admin, pinned to the commit that was checked", () => {
  const r = run({ gate: [0] });
  assert.equal(r.status, 0, r.out);
  assert.deepEqual(r.merges, [MERGE]);
  assert.deepEqual(r.reruns, []);
  assert.match(r.out, /MERGED PR #123/);
});

test("gate fails on a real failure: not merged, nothing re-run, exit 1", () => {
  const r = run({ gate: [1], jobs: [realJob, gateJob] });
  assert.equal(r.status, 1, r.out);
  assert.deepEqual(r.merges, []);
  assert.deepEqual(r.reruns, []);
  assert.match(r.out, /NOT MERGED/);
  assert.match(r.out, /real\tsecurity scan/);
});

test("the pipe that merged a failed gate: the exit status is lost, the merge still does not happen", () => {
  const r = run({ gate: [1], jobs: [realJob, gateJob], shell: "@SCRIPT@ 123 | tail -3 && echo PIPELINE-SAID-OK" });
  // `tail` succeeded, so the pipeline did; that is exactly why the merge lives inside the script.
  assert.match(r.out, /PIPELINE-SAID-OK/);
  assert.deepEqual(r.merges, []);
});

test("an infra failure is re-run once; a pass after it merges", () => {
  const r = run({ gate: [1, 0], jobs: [aptJob, gateJob], jobsAfter: [] });
  assert.equal(r.status, 0, r.out);
  assert.deepEqual(r.reruns, ["gh run rerun 77 --repo o/r --failed"]);
  assert.deepEqual(r.merges, [MERGE]);
  assert.equal(r.waits.length, 2);
});

test("an infra failure that fails again after the one re-run: not merged, no second re-run", () => {
  const r = run({ gate: [1, 1], jobs: [aptJob, gateJob] });
  assert.equal(r.status, 1, r.out);
  assert.equal(r.reruns.length, 1);
  assert.deepEqual(r.merges, []);
  assert.match(r.out, /failed again after one re-run/);
});

test("a real failure after the re-run of an infra one: not merged", () => {
  const r = run({ gate: [1, 1], jobs: [aptJob, gateJob], jobsAfter: [realJob, gateJob] });
  assert.equal(r.status, 1, r.out);
  assert.equal(r.reruns.length, 1);
  assert.deepEqual(r.merges, []);
});

test("infra beside a real failure is not re-run", () => {
  const r = run({ gate: [1], jobs: [aptJob, realJob, gateJob] });
  assert.equal(r.status, 1);
  assert.deepEqual([r.reruns.length, r.merges.length], [0, 0]);
});

test("a Playwright flaky-only failure is re-run once", () => {
  const log = "j\ts\t2026-11-18T07:14:42.0Z   1 flaky\nj\ts\t2026-11-18T07:14:42.1Z   107 passed (5.1m)\n";
  const r = run({ gate: [1, 0], jobs: [browserJob, gateJob], jobsAfter: [], log });
  assert.equal(r.status, 0, r.out);
  assert.equal(r.reruns.length, 1);
  assert.deepEqual(r.merges, [MERGE]);
  // The same job with a failed test in its summary is real.
  const real = run({ gate: [1], jobs: [browserJob, gateJob], log: "j\ts\t2026-11-18T07:14:42.0Z   1 failed\nj\ts\t2026-11-18T07:14:42.1Z   107 passed (5.1m)\n" });
  assert.deepEqual([real.status, real.reruns.length, real.merges.length], [1, 0, 0]);
});

test("'already running' from the re-run is tolerated (ci-runner-retry got there first)", () => {
  const r = run({ gate: [1, 0], jobs: [aptJob, gateJob], jobsAfter: [], rerunMode: "already" });
  assert.equal(r.status, 0, r.out);
  assert.deepEqual(r.merges, [MERGE]);
});

test("a re-run GitHub refuses for another reason: not merged", () => {
  const r = run({ gate: [1, 0], jobs: [aptJob, gateJob], rerunMode: "fail" });
  assert.equal(r.status, 1, r.out);
  assert.deepEqual(r.merges, []);
});

test("--no-rerun: an infra failure is final", () => {
  const r = run({ gate: [1, 0], jobs: [aptJob, gateJob], args: ["--no-rerun"] });
  assert.deepEqual([r.status, r.reruns.length, r.merges.length], [1, 0, 0]);
});

test("the wait timing out (exit 2) is not a pass", () => {
  const r = run({ gate: [2] });
  assert.equal(r.status, 2);
  assert.deepEqual(r.merges, []);
});

test("--no-merge: a pass is reported and nothing is merged", () => {
  const r = run({ gate: [0], args: ["--no-merge"] });
  assert.equal(r.status, 0);
  assert.deepEqual(r.merges, []);
});

test("a failed merge command is exit 3, and says NOT MERGED", () => {
  const r = run({ gate: [0], mergeRc: 1 });
  assert.equal(r.status, 3);
  assert.match(r.out, /NOT MERGED/);
});

test("a PR that is closed, a draft or already merged is never merged here", () => {
  const closed = run({ gate: [0], pr: { state: "CLOSED", isDraft: false, headRefOid: SHA } });
  assert.deepEqual([closed.status, closed.merges.length, closed.waits.length], [2, 0, 0]);
  const draft = run({ gate: [0], pr: { state: "OPEN", isDraft: true, headRefOid: SHA } });
  assert.deepEqual([draft.status, draft.merges.length], [2, 0]);
  const merged = run({ gate: [0], pr: { state: "MERGED", isDraft: false, headRefOid: SHA } });
  assert.deepEqual([merged.status, merged.merges.length], [0, 0]);
});

test("bad usage is exit 2 and calls nothing", () => {
  for (const args of [["--timeout-min"], ["--yes"]]) {
    const r = run({ gate: [0], args });
    assert.equal(r.status, 2);
    assert.deepEqual(r.merges, []);
  }
  const res = spawnSync("bash", [script, "abc"], { encoding: "utf8" });
  assert.equal(res.status, 2);
});
