#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import os from "node:os";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const full = process.argv.includes("--full");
const hardware = process.argv.includes("--hardware");
const skipBuild = process.argv.includes("--skip-build");

function run(label, command, args, opts = {}) {
  process.stdout.write(`\n==> ${label}\n`);
  const res = spawnSync(command, args, {
    cwd: opts.cwd || repoRoot,
    stdio: "inherit",
    shell: os.platform() === "win32",
    env: { ...process.env, ...opts.env },
  });
  if (res.status !== 0) {
    process.stderr.write(`\nvalidate-repo: ${label} failed with exit ${res.status}\n`);
    process.exit(res.status || 1);
  }
}

run("version sync", "node", ["scripts/update-version.js", "--check"]);
run("script syntax", "node", ["scripts/check-scripts.mjs"]);
run("script inventory", "node", ["scripts/audit-scripts.mjs"]);
run("i18n coverage", "node", ["scripts/i18n-coverage.mjs"]);
run("git whitespace", "git", ["diff", "--check"]);

run("engine fmt", "cargo", ["fmt", "--all", "--", "--check"], { cwd: path.join(repoRoot, "engine") });
run("engine clippy", "cargo", ["clippy", "--workspace", "--", "-D", "warnings"], { cwd: path.join(repoRoot, "engine") });
run("engine tests", "cargo", ["test", "--workspace"], { cwd: path.join(repoRoot, "engine") });

run("desktop cargo check", "cargo", ["check", "--all-targets"], { cwd: path.join(repoRoot, "client", "src-tauri") });
run("desktop clippy", "cargo", ["clippy", "--all-targets", "--", "-D", "warnings"], { cwd: path.join(repoRoot, "client", "src-tauri") });
run("desktop tests", "cargo", ["test"], { cwd: path.join(repoRoot, "client", "src-tauri") });

run("client typecheck", "npm", ["run", "typecheck"], { cwd: path.join(repoRoot, "client") });
run("client lint", "npm", ["run", "lint"], { cwd: path.join(repoRoot, "client") });
run("client tests", "npm", ["test"], { cwd: path.join(repoRoot, "client") });
if (!skipBuild) run("client vite build", "npm", ["run", "build:vite"], { cwd: path.join(repoRoot, "client") });

if (full) {
  run("payload ELF build/validation", "make", ["test-payload"]);
  run("root aggregate test", "make", ["test"]);
}

if (hardware) {
  run("live PS5 validate", "make", ["validate"]);
}

process.stdout.write("\nvalidate-repo: all selected checks passed\n");
