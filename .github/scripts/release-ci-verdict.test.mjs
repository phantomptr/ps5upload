import assert from "node:assert/strict";
import test from "node:test";
import { ownVerdict, verdict } from "./release-ci-verdict.mjs";

const ok = { status: "completed", conclusion: "success" };
const bad = { status: "completed", conclusion: "failure" };
const cancelled = { status: "completed", conclusion: "cancelled" };
const running = { status: "in_progress", conclusion: null };

// A little history: commits with a tree, a parent and the PR that produced them.
function world({ commits, runs = {}, prs = {}, ancestors = [] }) {
  return {
    runs: (sha) => runs[sha] ?? [],
    mergedPrs: (sha) => prs[sha] ?? [],
    tree: (sha) => commits[sha].tree,
    parent: (sha) => commits[sha]?.parent ?? null,
    isAncestor: (a, b) => ancestors.some(([x, y]) => x === a && y === b),
  };
}

test("ownVerdict", () => {
  assert.equal(ownVerdict([]), "absent");
  assert.equal(ownVerdict([bad, ok]), "pass");
  assert.equal(ownVerdict([bad, running]), "pending");
  assert.equal(ownVerdict([cancelled]), "cancelled");
  assert.equal(ownVerdict([cancelled, bad]), "fail");
});

test("a passing run on the commit itself", () => {
  const w = world({ commits: { P: { tree: "t1", parent: "M" } }, runs: { P: [ok] } });
  assert.equal(verdict("P", w).verdict, "pass");
});

test("main's run still going: the up-to-date PR head with the same tree stands in", () => {
  const w = world({
    commits: { P: { tree: "t2", parent: "M" }, M: { tree: "t1", parent: null }, H: { tree: "t2", parent: "M" } },
    runs: { P: [running], M: [ok], H: [ok] },
    prs: { P: [{ number: 7, head: "H" }] },
    ancestors: [["M", "H"]],
  });
  const v = verdict("P", w);
  assert.equal(v.verdict, "pass");
  assert.match(v.reason, /PR #7/);
});

test("the parent's run was cancelled by the next push: its own PR vouches for it", () => {
  const w = world({
    commits: {
      P: { tree: "t3", parent: "M" },
      M: { tree: "t2", parent: "L" },
      L: { tree: "t1", parent: null },
      H: { tree: "t3", parent: "M" },
      G: { tree: "t2", parent: "L" },
    },
    runs: { P: [running], M: [cancelled], L: [ok], H: [ok], G: [ok] },
    prs: { P: [{ number: 8, head: "H" }], M: [{ number: 7, head: "G" }] },
    ancestors: [["M", "H"], ["L", "G"]],
  });
  assert.equal(verdict("P", w).verdict, "pass");
});

test("a PR head that was behind main does not stand in (its run tested another tree)", () => {
  const w = world({
    commits: { P: { tree: "t2", parent: "M" }, M: { tree: "t1" }, H: { tree: "t2" } },
    runs: { P: [running], M: [ok], H: [ok] },
    prs: { P: [{ number: 7, head: "H" }] },
    ancestors: [],
  });
  assert.equal(verdict("P", w).verdict, "pending");
});

test("a different tree does not stand in", () => {
  const w = world({
    commits: { P: { tree: "t2", parent: "M" }, M: { tree: "t1" }, H: { tree: "t9" } },
    runs: { P: [], M: [ok], H: [ok] },
    prs: { P: [{ number: 7, head: "H" }] },
    ancestors: [["M", "H"]],
  });
  assert.equal(verdict("P", w).verdict, "absent");
});

test("a failed parent blocks the stand-in", () => {
  const w = world({
    commits: { P: { tree: "t2", parent: "M" }, M: { tree: "t1" }, H: { tree: "t2" } },
    runs: { P: [running], M: [bad], H: [ok] },
    prs: { P: [{ number: 7, head: "H" }] },
    ancestors: [["M", "H"]],
  });
  const v = verdict("P", w);
  assert.equal(v.verdict, "pending");
  assert.match(v.reason, /parent M is fail/);
});

test("a finished, failed run on the commit is final: no stand-in over it", () => {
  const w = world({
    commits: { P: { tree: "t2", parent: "M" }, M: { tree: "t1" }, H: { tree: "t2" } },
    runs: { P: [bad], M: [ok], H: [ok] },
    prs: { P: [{ number: 7, head: "H" }] },
    ancestors: [["M", "H"]],
  });
  assert.equal(verdict("P", w).verdict, "fail");
});

test("a PR head whose own ci failed does not stand in", () => {
  const w = world({
    commits: { P: { tree: "t2", parent: "M" }, M: { tree: "t1" }, H: { tree: "t2" } },
    runs: { P: [running], M: [ok], H: [bad] },
    prs: { P: [{ number: 7, head: "H" }] },
    ancestors: [["M", "H"]],
  });
  assert.equal(verdict("P", w).verdict, "pending");
});

test("the chain of stand-ins is bounded", () => {
  // Every commit's own run cancelled, each PR fine: never reaches a commit with its own pass.
  const commits = {};
  const runs = {};
  const prs = {};
  const ancestors = [];
  for (let i = 0; i < 20; i++) {
    commits[`C${i}`] = { tree: `t${i}`, parent: `C${i + 1}` };
    commits[`H${i}`] = { tree: `t${i}` };
    runs[`C${i}`] = [cancelled];
    runs[`H${i}`] = [ok];
    prs[`C${i}`] = [{ number: i, head: `H${i}` }];
    ancestors.push([`C${i + 1}`, `H${i}`]);
  }
  commits.C20 = { tree: "t20", parent: null };
  assert.equal(verdict("C0", world({ commits, runs, prs, ancestors })).verdict, "cancelled");
});
