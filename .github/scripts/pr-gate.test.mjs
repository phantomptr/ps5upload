import assert from "node:assert/strict";
import test from "node:test";
import { gateProblems, JOB_LANES } from "./pr-gate.mjs";
import { classify, outputLines } from "./ci-changes.mjs";

// The needs context for a change, with every job's result as the workflow would give it.
function needsFor(files, override = {}) {
  const outputs = Object.fromEntries(outputLines(classify(files)).map((l) => l.split("=")));
  const needs = { changes: { result: "success", outputs } };
  for (const [job, lane] of Object.entries(JOB_LANES)) {
    if (job === "changes") continue;
    const runs = lane === "always" || outputs[lane] === "true";
    needs[job] = { result: runs ? "success" : "skipped", outputs: {} };
  }
  for (const [job, result] of Object.entries(override)) needs[job] = { ...needs[job], result };
  return needs;
}

test("docs-only: the skipped jobs were skipped on purpose", () => {
  assert.deepEqual(gateProblems(needsFor(["README.md"])), []);
});

test("client-only, engine-only, payload-only, hub-only and a workflow change all pass when green", () => {
  for (const f of [
    "client/src/App.tsx",
    "engine/crates/ps5upload-engine/src/lib.rs",
    "payload/src/runtime.c",
    "engine/crates/ps5upload-hub/src/lib.rs",
    ".github/workflows/engine-ci.yml",
  ]) {
    assert.deepEqual(gateProblems(needsFor([f])), [], f);
  }
});

test("a needed job that failed fails the gate", () => {
  const p = gateProblems(needsFor(["engine/crates/ps5upload-engine/src/lib.rs"], { "rust-engine-tests": "failure" }));
  assert.equal(p.length, 1);
  assert.match(p[0], /rust-engine-tests: failure/);
});

test("a needed job skipped because what it needs failed fails the gate", () => {
  const p = gateProblems(needsFor(["payload/src/runtime.c"], { payload: "failure", "payload-selftests": "skipped" }));
  assert.equal(p.length, 2);
  assert.ok(p.some((x) => /payload-selftests: skipped/.test(x)));
});

test("a cancelled job fails the gate, needed or not", () => {
  assert.equal(gateProblems(needsFor(["client/src/App.tsx"], { "client-frontend": "cancelled" })).length, 1);
  assert.equal(gateProblems(needsFor(["README.md"], { android: "cancelled" })).length, 1);
});

test("an always-on job skipped fails the gate", () => {
  assert.equal(gateProblems(needsFor(["README.md"], { "security-scan": "skipped" })).length, 1);
});

test("when the changes job fails, nothing can pass", () => {
  const needs = needsFor(["README.md"]);
  needs.changes = { result: "failure", outputs: {} };
  for (const job of Object.keys(needs)) if (job !== "changes") needs[job].result = "skipped";
  const p = gateProblems(needs);
  assert.equal(p.length, 1);
  assert.match(p[0], /changes: failure/);
});

test("a job the gate does not know fails it", () => {
  const needs = needsFor(["README.md"]);
  needs["new-job"] = { result: "success", outputs: {} };
  assert.match(gateProblems(needs)[0], /new-job: not in pr-gate.mjs/);
});

test("a job left out of the gate's needs fails it", () => {
  const needs = needsFor(["README.md"]);
  delete needs.android;
  assert.match(gateProblems(needs)[0], /android: missing/);
});

test("a lane output that is not true/false fails it", () => {
  const needs = needsFor(["README.md"]);
  delete needs.changes.outputs.engine;
  assert.ok(gateProblems(needs).some((p) => /lane 'engine' has no true\/false/.test(p)));
});
