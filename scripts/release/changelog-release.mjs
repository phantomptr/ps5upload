#!/usr/bin/env node
// Gives CHANGELOG.md its "## <version>" heading for a release (release-tag.yml, on the commit it
// tags). The entry is written beforehand, in the batch PR, under "## Unreleased".
//
//   node scripts/release/changelog-release.mjs <x.y.z> [--previous <a.b.c>]
//
// - "## <version>" already there with text under it: nothing to do.
// - "## Unreleased" there with text under it: renamed to "## <version>".
// - Otherwise: an error, and nothing is written (publish.yml takes the release notes from that
//   section and refuses an empty one after the whole build).
// --previous: the last released version. Its section must already be in the file: after a release, main still says "## Unreleased" over that release's text until
// the next batch PR renames it, and releasing then would ship the old notes as the new ones.

import fs from "node:fs";
import path from "node:path";

const SEMVER = /^\d+\.\d+\.\d+$/;

/** The text under a `## <name>` heading, up to the next `## ` or `---` line; null if absent. */
export function section(md, name) {
  const lines = md.split("\n");
  const start = lines.findIndex((l) => l === `## ${name}`);
  if (start < 0) return null;
  const body = [];
  for (const line of lines.slice(start + 1)) {
    if (line.startsWith("## ") || /^---\s*$/.test(line)) break;
    body.push(line);
  }
  return body.join("\n");
}

const filled = (text) => text !== null && text.trim() !== "";

/** Returns {md, changed} or throws with the reason. */
export function release(md, version, previous) {
  if (!SEMVER.test(version)) throw new Error(`'${version}' is not X.Y.Z`);
  if (previous !== undefined && previous !== "") {
    if (!SEMVER.test(previous)) throw new Error(`--previous '${previous}' is not X.Y.Z`);
    if (section(md, previous) === null) {
      throw new Error(
        `CHANGELOG.md has no '## ${previous}' section: main has not caught up with v${previous} yet. ` +
          `In a PR, rename its '## Unreleased' to '## ${previous}' (or take CHANGELOG.md from the v${previous} tag), ` +
          `write the new entry under a new '## Unreleased' above it, merge, and dispatch again.`,
      );
    }
  }
  const own = section(md, version);
  if (filled(own)) return { md, changed: false };
  if (own !== null) throw new Error(`CHANGELOG.md's '## ${version}' section is empty`);
  if (!filled(section(md, "Unreleased"))) {
    throw new Error(`CHANGELOG.md has no '## ${version}' section and no '## Unreleased' section with text`);
  }
  const lines = md.split("\n");
  const at = lines.findIndex((l) => l === "## Unreleased");
  if (lines.indexOf("## Unreleased", at + 1) >= 0) throw new Error("CHANGELOG.md has two '## Unreleased' headings");
  lines[at] = `## ${version}`;
  return { md: lines.join("\n"), changed: true };
}

if (process.argv[1]?.endsWith("changelog-release.mjs")) {
  const args = process.argv.slice(2);
  const version = args.find((a, i) => !a.startsWith("--") && args[i - 1] !== "--previous");
  const p = args.indexOf("--previous");
  const previous = p >= 0 ? args[p + 1] : undefined;
  const file = path.join(import.meta.dirname, "..", "..", "CHANGELOG.md");
  try {
    const { md, changed } = release(fs.readFileSync(file, "utf8"), version ?? "", previous);
    if (changed) fs.writeFileSync(file, md);
    process.stdout.write(changed ? `CHANGELOG.md: '## Unreleased' is now '## ${version}'\n` : `CHANGELOG.md already has '## ${version}'\n`);
  } catch (err) {
    process.stderr.write(`changelog-release: ${err.message}\n`);
    process.exit(1);
  }
}
