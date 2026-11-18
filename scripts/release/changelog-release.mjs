#!/usr/bin/env node
// Gives CHANGELOG.md its "## <version>" heading for a release (release-tag.yml, on the commit it
// tags). The entry is written beforehand, in the batch PR, under "## Unreleased".
//
//   node scripts/release/changelog-release.mjs <x.y.z> [--previous <a.b.c>] --check   # reads only
//   node scripts/release/changelog-release.mjs <x.y.z> [--previous <a.b.c>]           # REWRITES the file
//
// WITHOUT --check THIS WRITES CHANGELOG.md: it renames "## Unreleased" to "## <version>" in place.
// Only release-tag.yml wants that (on a commit that goes to the tag, never to main). To ask "would
// this release be accepted?" from a checkout, a script or a PR, always pass --check: same rules,
// same exit status, nothing written (a check run without it once renamed main's "## Unreleased").
//
// - "## <version>" already there with text under it: nothing to do.
// - "## Unreleased" there with text under it: renamed to "## <version>" (--check: only says so).
// - Otherwise: an error, and nothing is written (publish.yml takes the release notes from that
//   section and refuses an empty one after the whole build).
// --previous: the last released version. Its section must already be in the file: after a release, main still says "## Unreleased" over that release's text until
// the next batch PR renames it, and releasing then would ship the old notes as the new ones.
// Exit status: 0 fine, 1 the CHANGELOG is not ready for this release, 2 bad arguments.

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

export const USAGE = `usage: changelog-release.mjs <x.y.z> [--previous <a.b.c>] [--check]

  --check           read only: say whether CHANGELOG.md is ready for <x.y.z>; write nothing
  --previous a.b.c  the last released version; its '## a.b.c' section must be in the file
  -h, --help        this text

Without --check this REWRITES CHANGELOG.md in place ('## Unreleased' becomes '## <x.y.z>').
That is release-tag.yml's job. Anything that only validates must pass --check.
`;

/** Returns {version, previous, check, help} or throws on anything it does not know. */
export function parseArgs(args) {
  const out = { version: undefined, previous: undefined, check: false, help: false };
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--check") out.check = true;
    else if (a === "-h" || a === "--help") out.help = true;
    else if (a === "--previous") {
      // An empty value is "no previous release" (release-tag.yml passes "" for the first one).
      if (i + 1 >= args.length) throw new Error("--previous needs a version");
      out.previous = args[++i];
    } else if (a.startsWith("-")) throw new Error(`unknown option '${a}'`);
    else if (out.version === undefined) out.version = a;
    else throw new Error(`unexpected argument '${a}'`);
  }
  if (!out.help && out.version === undefined) throw new Error("no version given");
  return out;
}

/**
 * The command: reads `file`, writes it only when `check` is off and the heading changed.
 * Returns {code, out, err}; `io` is {read(file), write(file, text)}.
 */
export function main(args, file, io) {
  let opts;
  try {
    opts = parseArgs(args);
  } catch (err) {
    return { code: 2, out: "", err: `changelog-release: ${err.message}\n${USAGE}` };
  }
  if (opts.help) return { code: 0, out: USAGE, err: "" };
  try {
    const { md, changed } = release(io.read(file), opts.version, opts.previous);
    if (!changed) return { code: 0, out: `CHANGELOG.md already has '## ${opts.version}'\n`, err: "" };
    if (opts.check) {
      return {
        code: 0,
        out: `CHANGELOG.md is ready: '## Unreleased' would become '## ${opts.version}' (--check: nothing written)\n`,
        err: "",
      };
    }
    io.write(file, md);
    return { code: 0, out: `CHANGELOG.md REWRITTEN: '## Unreleased' is now '## ${opts.version}'\n`, err: "" };
  } catch (err) {
    return { code: 1, out: "", err: `changelog-release: ${err.message}\n` };
  }
}

if (process.argv[1]?.endsWith("changelog-release.mjs")) {
  const file = path.join(import.meta.dirname, "..", "..", "CHANGELOG.md");
  const res = main(process.argv.slice(2), file, {
    read: (f) => fs.readFileSync(f, "utf8"),
    write: (f, text) => fs.writeFileSync(f, text),
  });
  if (res.out) process.stdout.write(res.out);
  if (res.err) process.stderr.write(res.err);
  process.exit(res.code);
}
