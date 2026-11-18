#!/usr/bin/env node
// Has `ci` vouched for this commit? release-tag.yml asks before it tags a release on top of it.
//
//   node .github/scripts/release-ci-verdict.mjs <sha>       prints {"verdict", "reason"} as JSON
//
// Verdicts: pass, pending (a run is still going), absent (no run yet), cancelled (every run was
// cancelled or skipped: none of them tested anything), fail.
//
// A commit passes when a `ci` run on it succeeded, or when its tree is the tree a passing run
// already tested: the merged pull request's head, provided
//   - the head's tree is this commit's tree (`git rev-parse <sha>^{tree}`),
//   - this commit's parent is an ancestor of the head (the branch was up to date, so the PR's
//     run tested exactly this tree: its merge commit was the head itself), and
//   - the parent passes too, by the same rule. A PR run only runs the lanes its change touches
//     (ci-changes.mjs), so it vouches for the change on top of a parent that was already good.
//     Not needed when the PR's change turned every lane on (a workflow, a lockfile, VERSION: every
//     batch PR after a release): that run ran exactly the jobs main's own run runs, on this
//     tree, so it vouches for the whole tree whatever the parent's run said. The lanes come from
//     the same classifier the PR run used (ci-changes.mjs) over the parent..commit diff, which
//     is the PR run's own diff (an up-to-date PR's merge commit has the parent as its base).
// A commit whose own run finished and failed does not pass on a PR's result.
//
// Main's own run of the merge commit still runs every lane afterwards.

import { execFileSync } from "node:child_process";
import { classify } from "./ci-changes.mjs";

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
  if (runs.every((r) => NO_VERDICT.has(r.conclusion))) return "cancelled";
  return "fail";
}

/**
 * Conclusions of a finished run that tested nothing: cancelled (the next push to main), skipped
 * (every job's `if` was off), stale and neutral. Like a cancel, they neither pass the commit nor
 * count as its failure, so a pull request's result may still stand in. Anything else that is
 * not `success` (failure, timed_out, startup_failure, action_required) is a failure.
 */
export const NO_VERDICT = new Set(["cancelled", "skipped", "stale", "neutral"]);

/**
 * deps: {runs(sha), mergedPrs(sha) -> [{number, head}], tree(sha), parent(sha) -> sha|null,
 *        isAncestor(a, b), everyLane(base, head) -> did a PR run of base..head run every lane}
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
      if (deps.everyLane?.(parent, sha)) {
        return {
          verdict: "pass",
          reason: `${sha} has the tree of ${tag}, whose ci passed with every lane on (the parent's run is not needed)`,
        };
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
    // Not the nightly `schedule` runs: they only make the coverage reports (every gating job is
    // off), so their success says nothing about the commit.
    runs: (sha) =>
      JSON.parse(
        sh("gh", [
          "api",
          `repos/${repo}/actions/workflows/${workflowFile}/runs?head_sha=${sha}&per_page=100`,
          "--jq",
          '[.workflow_runs[] | select(.event != "schedule") | {status, conclusion}]',
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
    // The rules are the checked-out tree's: the commit being tagged, so for it they are exactly
    // the ones its PR run used (for an ancestor further up, possibly newer ones).
    everyLane: (base, head) =>
      classify(sh("git", ["diff", "--no-renames", "--name-only", base, head]).split("\n")).all,
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
