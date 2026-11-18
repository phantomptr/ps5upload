import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_WAIT_MINUTES, LISTING_GRACE_MS, decide, limitMs } from "./wait-for-artifact.mjs";

const MIN = 60_000;
const base = {
  artifact: "ps5upload-payload",
  producer: "build-payload",
  artifactListed: false,
  startedAt: 0,
  limitMs: limitMs(undefined),
  successSeenAt: null,
};
const at = (min, job, extra = {}) => decide({ ...base, now: min * MIN, job, ...extra });

test("the artifact being listed ends the wait, whatever the job says", () => {
  assert.equal(at(1, null, { artifactListed: true }).action, "ready");
  assert.equal(at(99, { status: "completed", conclusion: "failure" }, { artifactListed: true }).action, "ready");
});

test("v12.6.7: build-payload queued ~20 min for a runner is waited for, not failed", () => {
  // publish run 44253579748, attempt 1: macOS x64 gave up at 20 min while the payload job was queued.
  for (const min of [1, 19, 21, 30, 44]) {
    const d = at(min, { status: "queued", conclusion: null });
    assert.equal(d.action, "wait", `minute ${min}`);
    assert.match(d.message, /waiting for a runner/);
  }
  assert.equal(at(25, { status: "in_progress", conclusion: null }).action, "wait");
});

test("a payload job that finished without success fails at once", () => {
  for (const conclusion of ["failure", "cancelled", "timed_out", "skipped"]) {
    const d = at(0.5, { status: "completed", conclusion });
    assert.equal(d.action, "fail", conclusion);
    assert.match(d.message, new RegExp(`finished '${conclusion}'`));
  }
});

test("the limit ends a job still queued or running, and says which", () => {
  const queued = at(DEFAULT_WAIT_MINUTES + 1, { status: "queued", conclusion: null });
  assert.equal(queued.action, "fail");
  assert.match(queued.message, /did not appear in time.*waiting for a runner/);
  const running = at(DEFAULT_WAIT_MINUTES + 1, { status: "in_progress", conclusion: null });
  assert.match(running.message, /did not appear in time.*is running/);
  const unlisted = at(DEFAULT_WAIT_MINUTES + 1, null);
  assert.match(unlisted.message, /not in this run's job list/);
});

test("a successful job gets a short grace for its artifact to be listed, then fails", () => {
  const ok = { status: "completed", conclusion: "success" };
  const first = at(5, ok);
  assert.equal(first.action, "wait");
  assert.equal(first.successSeenAt, 5 * MIN);
  assert.equal(at(7, ok, { successSeenAt: first.successSeenAt }).action, "wait");
  const tooLate = decide({ ...base, now: 5 * MIN + LISTING_GRACE_MS + 1, job: ok, successSeenAt: first.successSeenAt });
  assert.equal(tooLate.action, "fail");
  assert.match(tooLate.message, /succeeded but artifact/);
});

test("a re-run that puts the job back in the queue restarts the grace", () => {
  const d = at(6, { status: "queued", conclusion: null }, { successSeenAt: 5 * MIN });
  assert.equal(d.action, "wait");
  assert.equal(d.successSeenAt, null);
});

test("PS5UPLOAD_WAIT_MINUTES: a positive number, else the default", () => {
  assert.equal(limitMs("10"), 10 * MIN);
  assert.equal(limitMs(undefined), DEFAULT_WAIT_MINUTES * MIN);
  assert.equal(limitMs("abc"), DEFAULT_WAIT_MINUTES * MIN);
  assert.equal(limitMs("0"), DEFAULT_WAIT_MINUTES * MIN);
});
