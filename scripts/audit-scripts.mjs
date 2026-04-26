#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const strict = process.argv.includes("--strict");

const manualEntrypoints = new Set([
  "scripts/bundle-fonts.py",
  "scripts/hw-test-loop.sh",
  "scripts/pull_ps5_debug.py",
  "scripts/release-posts.js",
  "scripts/translate-i18n.py",
  "tests/lab/README.md",
  "tests/lab/test_launch_only.py",
]);

function gitLsFiles(prefixes) {
  const res = spawnSync("git", ["ls-files", ...prefixes], {
    cwd: repoRoot,
    encoding: "utf8",
  });
  if (res.status !== 0) throw new Error(res.stderr || "git ls-files failed");
  return res.stdout.trim().split(/\n/).filter(Boolean);
}

function countRefs(needle, haystackFiles) {
  const base = path.basename(needle);
  let refs = 0;
  for (const file of haystackFiles) {
    if (file === "scripts/audit-scripts.mjs") continue;
    try {
      const text = fs.readFileSync(path.join(repoRoot, file), "utf8");
      if (text.includes(base)) refs += 1;
    } catch {
      // Binary or unreadable tracked file: irrelevant for script references.
    }
  }
  return refs;
}

const files = gitLsFiles(["scripts", "tests/lab", "bench"]);
const haystackFiles = gitLsFiles(["."]).filter((file) =>
  !file.startsWith("client/node_modules/")
  && !file.startsWith("engine/target/")
  && !file.startsWith("client/src-tauri/target/")
  && !file.startsWith("client/dist/")
);
const executableLike = files.filter((f) => /\.(mjs|js|py|sh|ps1)$/i.test(f));
const rows = executableLike.map((file) => ({
  file,
  refs: countRefs(file, haystackFiles),
  manual: manualEntrypoints.has(file),
}));
const suspicious = rows.filter((r) => r.refs === 0 && !r.manual);

process.stdout.write("[audit-scripts] tracked script/test utilities\n");
for (const r of rows) {
  const mark = r.manual ? "manual" : r.refs === 0 ? "orphan?" : "used";
  process.stdout.write(`${String(r.refs).padStart(3)}  ${mark.padEnd(7)}  ${r.file}\n`);
}

if (suspicious.length > 0) {
  process.stdout.write("\n[audit-scripts] review these unreferenced files:\n");
  for (const r of suspicious) process.stdout.write(`  ${r.file}\n`);
  if (strict) process.exit(1);
}
