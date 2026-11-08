import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const script = path.join(import.meta.dirname, "flake-hunt.sh");
// GNU timeout: on the Linux runner the workflow uses; macOS has it only with coreutils.
const usable =
  process.platform !== "win32" && spawnSync("sh", ["-c", "command -v timeout"], { stdio: "ignore" }).status === 0;

function hunt(rounds, budget, ...cmd) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "flake-hunt-"));
  const res = spawnSync("bash", [script, dir, "set", String(rounds), String(budget), "--", ...cmd], {
    encoding: "utf8",
  });
  const read = (f) => (fs.existsSync(path.join(dir, f)) ? fs.readFileSync(path.join(dir, f), "utf8") : "");
  const out = { status: res.status, stdout: res.stdout, summary: read("summary.md"), failures: read("failures.md") };
  fs.rmSync(dir, { recursive: true, force: true });
  return out;
}

test("all rounds pass: a summary row, no failures", { skip: !usable }, () => {
  const r = hunt(3, 60, "true");
  assert.equal(r.status, 0);
  assert.equal(r.summary.trim(), "| set | 3 | 0 |  |");
  assert.equal(r.failures, "");
});

test("every round fails: counted, with the output, and still exit 0", { skip: !usable }, () => {
  const r = hunt(2, 60, "sh", "-c", "echo boom; exit 3");
  assert.equal(r.status, 0);
  assert.equal(r.summary.trim(), "| set | 2 | 2 |  |");
  assert.match(r.failures, /### set, round 1: exit 3/);
  assert.match(r.failures, /boom/);
});

test("a flaky command: only the failing rounds are reported", { skip: !usable }, () => {
  const state = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "flaky-")), "n");
  // Fails on the 2nd round only.
  const cmd = `n=$(cat "${state}" 2>/dev/null || echo 0); n=$((n+1)); echo $n > "${state}"; [ $n -ne 2 ]`;
  const r = hunt(4, 60, "sh", "-c", cmd);
  assert.equal(r.summary.trim(), "| set | 4 | 1 |  |");
  assert.match(r.failures, /round 2: exit 1/);
  assert.doesNotMatch(r.failures, /round [134]/);
});

test("a round the budget cuts off is a note, not a failure", { skip: !usable }, () => {
  const r = hunt(5, 1, "sleep", "5");
  assert.equal(r.status, 0);
  assert.match(r.summary, /\| set \| 0 \| 0 \| round 1 cut off by the time budget \|/);
  assert.equal(r.failures, "");
});

test("bad arguments", { skip: process.platform === "win32" }, () => {
  const res = spawnSync("bash", [script, "x", "y"], { encoding: "utf8" });
  assert.equal(res.status, 2);
});
