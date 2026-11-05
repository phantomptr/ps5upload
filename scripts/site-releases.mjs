#!/usr/bin/env node
// Writes site/releases.json: the website's fallback copy of the newest releases, used when a
// visitor's browser can't reach GitHub's API (offline, or over its anonymous rate limit).
// Run by .github/workflows/pages.yml on every deploy and every published release; run it by
// hand to refresh a local preview:  node scripts/site-releases.mjs
//
// Uses `gh` when it is installed and signed in (no rate limit worth worrying about), else
// GITHUB_TOKEN with fetch, else an anonymous fetch.
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const REPO = "phantomptr/ps5upload";
const out = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "site", "releases.json");

/** Only what the page reads, so the snapshot stays small. */
const slim = (list, count) =>
  list
    .filter((r) => !r.draft)
    .slice(0, count)
    .map((r) => ({
      tag_name: r.tag_name,
      prerelease: r.prerelease,
      published_at: r.published_at,
      html_url: r.html_url,
      body: r.body,
      assets: r.assets.map((a) => ({ name: a.name, browser_download_url: a.browser_download_url, size: a.size })),
    }));

async function fetchReleases(which = "?per_page=12") {
  try {
    const json = execFileSync("gh", ["api", `repos/${REPO}/releases${which}`], {
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    return JSON.parse(json);
  } catch {
    const headers = { Accept: "application/vnd.github+json" };
    if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases${which}`, { headers });
    if (!res.ok) throw new Error(`GitHub answered ${res.status}`);
    return res.json();
  }
}

const all = await fetchReleases();
// The page offers the newest stable release first. With a run of pre-releases it can fall out of
// the newest twelve, so it is added at the end.
if (!all.some((r) => !r.draft && !r.prerelease)) {
  const latest = await fetchReleases("/latest").catch(() => null);
  if (latest?.tag_name) all.push(latest);
}
const list = slim(all, 13);
writeFileSync(out, `${JSON.stringify(list, null, 1)}\n`);
console.log(`site/releases.json: ${list.length} releases, newest ${list[0]?.tag_name ?? "none"}`);
