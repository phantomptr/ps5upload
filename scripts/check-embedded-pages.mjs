#!/usr/bin/env node
// The engine embeds two built pages, committed as files: join_page.html (Play Together guests)
// and pad_page.html (the phone controller). The website carries a copy of each too,
// site/join/index.html and site/pad/index.html, where internet links open. A source or translation edit without a
// rebuild passes every other check and never reaches a phone, so this rebuilds them and fails if a
// file changed (or the site's copy is not the engine's page byte for byte).
// The rebuilt files stay in the tree: after a failure, commit them.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const pages = [
  ["build:join", ["engine/crates/ps5upload-engine/src/join_page.html", "site/join/index.html"]],
  ["build:pad", ["engine/crates/ps5upload-engine/src/pad_page.html", "site/pad/index.html"]],
];

const stale = [];
for (const [script, files] of pages) {
  const before = files.map((f) => {
    const abs = path.join(repoRoot, f);
    return [f, abs, fs.existsSync(abs) ? fs.statSync(abs) : null];
  });
  const build = spawnSync("npm", ["run", script], {
    cwd: path.join(repoRoot, "client"),
    stdio: "inherit",
    shell: os.platform() === "win32",
  });
  if (build.status !== 0) {
    process.stderr.write(`embedded pages: npm run ${script} failed with exit ${build.status}\n`);
    process.exit(build.status || 1);
  }
  for (const [file, abs, stat] of before) {
    // Untracked (never committed) counts as stale too.
    const tracked = spawnSync("git", ["ls-files", "--error-unmatch", "--", file], { cwd: repoRoot }).status === 0;
    const diff = spawnSync("git", ["diff", "--quiet", "--", file], { cwd: repoRoot });
    if (!tracked || diff.status !== 0) stale.push([script, file]);
    // Same bytes: keep the old time, or cargo recompiles the engine crate after every check.
    else if (stat) fs.utimesSync(abs, stat.atime, stat.mtime);
  }
  // Every output of one build is the same page.
  const [first, ...rest] = files.map((f) => fs.readFileSync(path.join(repoRoot, f)));
  for (const [i, b] of rest.entries()) {
    if (!b.equals(first)) {
      process.stderr.write(`embedded pages: ${files[i + 1]} differs from ${files[0]}\n`);
      process.exit(1);
    }
  }
}

if (stale.length) {
  for (const [script, file] of stale) {
    process.stderr.write(`embedded pages: ${file} is stale. Run npm run ${script} in client/ and commit it.\n`);
  }
  process.exit(1);
}
process.stdout.write("embedded pages: all match their sources\n");
