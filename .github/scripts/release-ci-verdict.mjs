#!/usr/bin/env node
// Has `ci` vouched for this commit? release-tag.yml asks before it tags a release on top of it.
//
//   node .github/scripts/release-ci-verdict.mjs <sha>       prints {"verdict", "reason"} as JSON
//
// Verdicts: pass, pending (a run is still going), absent (no run yet), cancelled (every run was
// cancelled), fail.
//
// A commit passes when a `ci` run on it succeeded, or when its tree is the tree a passing run
// already tested: the merged pull request's head, provided
//   - the head's tree is this commit's tree (`git rev-parse <sha>^{tree}`),
//   - this commit's parent is an ancestor of the head (the branch was up to date, so the PR's
//     run tested exactly this tree: its merge commit was the head itself), and
//   - the parent passes too, by the same rule. A PR run only runs the lanes its change touches
//     (ci-changes.mjs), so it vouches for the change on top of a parent that was already good.
// A commit whose own run finished and failed does not pass on a PR's result.
//
// Main's own run of the merge commit still runs every lane afterwards.

import { execFileSync } from "node:child_process";

const MAX_DEPTH = 8;

/**
 * The verdict for one commit's own `ci` runs: [{status, conclusion}]. `cancelled`: every run was
 * cancelled (on main, the next push cancels the previous commit's run), which says nothing about
 * the code, so a pull request's result may still stand in for it.
 */
export function ownVerdict(runs) {
  if (runs.some((r) => r.conclusion === "success")) return "pass";
  if (runs.some((r) => r.status !== "completed")) return "pending";
  if (runs.length === 0) return "absent";
  if (runs.every((r) => r.conclusion === "cancelled")) return "cancelled";
  return "fail";
}

/**
 * deps: {runs(sha), mergedPrs(sha) -> [{number, head}], tree(sha), parent(sha) -> sha|null,
 *        isAncestor(a, b)}
 */
export function verdict(sha, deps, depth = MAX_DEPTH) {
  const own = ownVerdict(deps.runs(sha));
  if (own === "pass") return { verdict: "pass", reason: `ci passed on ${sha}` };
  if (own === "fail") return { verdict: "fail", reason: `ci failed on ${sha}` };
  const notes = [];
  if (depth > 0) {
    const parent = deps.parent(sha);
    for (const pr of deps.mergedPrs(sha)) {
      const tag = `PR #${pr.number} head ${pr.head}`;
      if (deps.tree(pr.head) !== deps.tree(sha)) {
        notes.push(`${tag}: different tree`);
        continue;
      }
      if (!parent || !deps.isAncestor(parent, pr.head)) {
        notes.push(`${tag}: was not up to date with ${parent ?? "the parent"}`);
        continue;
      }
      const head = ownVerdict(deps.runs(pr.head));
      if (head !== "pass") {
        notes.push(`${tag}: ci ${head}`);
        continue;
      }
      const up = verdict(parent, deps, depth - 1);
      if (up.verdict === "pass") {
        return {
          verdict: "pass",
          reason: `${sha} has the tree of ${tag}, whose ci passed; parent: ${up.reason}`,
        };
      }
      notes.push(`${tag}: ci passed, but the parent ${parent} is ${up.verdict} (${up.reason})`);
    }
  }
  const reason = `ci ${own} on ${sha}` + (notes.length ? `; ${notes.join("; ")}` : "");
  return { verdict: own, reason };
}

// ─── The real lookups: gh for runs and pull requests, git for trees and ancestry ───

function sh(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], ...opts }).trim();
}

export function realDeps(repo, workflowFile = "engine-ci.yml") {
  const fetched = new Set();
  const ensure = (sha, number) => {
    if (fetched.has(sha)) return;
    try {
      sh("git", ["cat-file", "-e", `${sha}^{commit}`]);
    } catch {
      // The PR's head is not on main's history: fetch it (GitHub keeps refs/pull/N/head).
      sh("git", ["fetch", "--no-tags", "--quiet", "origin", `+refs/pull/${number}/head:refs/ci-verdict/pr-${number}`]);
    }
    fetched.add(sha);
  };
  return {
    runs: (sha) =>
      JSON.parse(
        sh("gh", [
          "api",
          `repos/${repo}/actions/workflows/${workflowFile}/runs?head_sha=${sha}&per_page=100`,
          "--jq",
          "[.workflow_runs[] | {status, conclusion}]",
        ]),
      ),
    mergedPrs: (sha) => {
      const prs = JSON.parse(
        sh("gh", [
          "api",
          `repos/${repo}/commits/${sha}/pulls`,
          "--jq",
          `[.[] | select(.merge_commit_sha == "${sha}") | {number, head: .head.sha}]`,
        ]),
      );
      for (const pr of prs) ensure(pr.head, pr.number);
      return prs;
    },
    tree: (sha) => sh("git", ["rev-parse", `${sha}^{tree}`]),
    parent: (sha) => {
      try {
        return sh("git", ["rev-parse", `${sha}^1`]);
      } catch {
        return null;
      }
    },
    isAncestor: (a, b) => {
      try {
        sh("git", ["merge-base", "--is-ancestor", a, b]);
        return true;
      } catch {
        return false;
      }
    },
  };
}

if (process.argv[1]?.endsWith("release-ci-verdict.mjs")) {
  const sha = process.argv[2];
  const repo = process.env.GITHUB_REPOSITORY;
  if (!/^[0-9a-f]{40}$/.test(sha ?? "") || !repo) {
    process.stderr.write("usage: GITHUB_REPOSITORY=owner/name release-ci-verdict.mjs <40-hex sha>\n");
    process.exit(2);
  }
  let result;
  try {
    result = verdict(sha, realDeps(repo));
  } catch (err) {
    // An API or git hiccup is not a verdict: the caller asks again.
    result = { verdict: "pending", reason: `lookup failed: ${String(err.message ?? err).split("\n")[0]}` };
  }
  process.stdout.write(`${JSON.stringify(result)}\n`);
}
