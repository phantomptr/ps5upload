#!/usr/bin/env node
// The engine embeds two built pages, committed as files: join_page.html (Play Together guests)
// and pad_page.html (the phone controller). A source or translation edit without a rebuild passes
// every other check and never reaches a phone, so this rebuilds both and fails if a file changed.
// The rebuilt files stay in the tree: after a failure, commit them.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const pages = [
  ["build:join", "engine/crates/ps5upload-engine/src/join_page.html"],
  ["build:pad", "engine/crates/ps5upload-engine/src/pad_page.html"],
];

const stale = [];
for (const [script, file] of pages) {
  const abs = path.join(repoRoot, file);
  const before = fs.statSync(abs);
  const build = spawnSync("npm", ["run", script], {
    cwd: path.join(repoRoot, "client"),
    stdio: "inherit",
    shell: os.platform() === "win32",
  });
  if (build.status !== 0) {
    process.stderr.write(`embedded pages: npm run ${script} failed with exit ${build.status}\n`);
    process.exit(build.status || 1);
  }
  const diff = spawnSync("git", ["diff", "--quiet", "--", file], { cwd: repoRoot });
  if (diff.status !== 0) stale.push([script, file]);
  // Same bytes: keep the old time, or cargo recompiles the engine crate after every check.
  else fs.utimesSync(abs, before.atime, before.mtime);
}

if (stale.length) {
  for (const [script, file] of stale) {
    process.stderr.write(`embedded pages: ${file} is stale. Run npm run ${script} in client/ and commit it.\n`);
  }
  process.exit(1);
}
process.stdout.write("embedded pages: both match their sources\n");
