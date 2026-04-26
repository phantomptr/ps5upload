#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const coverageRoot = path.join(repoRoot, "coverage");
const installTools = process.argv.includes("--install-tools");
const clientOnly = process.argv.includes("--client-only");
const engineOnly = process.argv.includes("--engine-only");

function run(label, command, args, opts = {}) {
  process.stdout.write(`\n==> ${label}\n`);
  const res = spawnSync(command, args, {
    cwd: opts.cwd || repoRoot,
    stdio: "inherit",
    shell: os.platform() === "win32",
    env: { ...process.env, ...opts.env },
  });
  if (res.status !== 0) {
    process.stderr.write(`\ncoverage: ${label} failed with exit ${res.status}\n`);
    process.exit(res.status || 1);
  }
}

function existsOnPath(bin) {
  const cmd = os.platform() === "win32" ? "where" : "command";
  const args = os.platform() === "win32" ? [bin] : ["-v", bin];
  return spawnSync(cmd, args, { shell: true, stdio: "ignore" }).status === 0;
}

function firstExisting(paths) {
  return paths.find((p) => fs.existsSync(p)) || null;
}

function llvmToolEnv() {
  if (process.env.LLVM_COV && process.env.LLVM_PROFDATA) return {};
  if (existsOnPath("llvm-cov") && existsOnPath("llvm-profdata")) return {};

  const prefixes = os.platform() === "darwin"
    ? ["/opt/homebrew/opt/llvm/bin", "/opt/homebrew/opt/llvm@22/bin", "/opt/homebrew/opt/llvm@18/bin", "/usr/local/opt/llvm/bin"]
    : ["/usr/lib/llvm-18/bin", "/usr/lib/llvm-17/bin", "/usr/lib/llvm-16/bin", "/usr/bin"];
  const llvmCov = firstExisting(prefixes.map((p) => path.join(p, "llvm-cov")));
  const llvmProfdata = firstExisting(prefixes.map((p) => path.join(p, "llvm-profdata")));
  if (llvmCov && llvmProfdata) {
    return {
      LLVM_COV: process.env.LLVM_COV || llvmCov,
      LLVM_PROFDATA: process.env.LLVM_PROFDATA || llvmProfdata,
    };
  }
  return {};
}

fs.mkdirSync(coverageRoot, { recursive: true });

if (!engineOnly) {
  run("client vitest coverage", "npm", ["run", "test:coverage"], {
    cwd: path.join(repoRoot, "client"),
  });
}

if (clientOnly) {
  process.stdout.write("\ncoverage reports:\n");
  process.stdout.write(`  client: ${path.relative(repoRoot, path.join(repoRoot, "client", "coverage", "index.html"))}\n`);
  process.exit(0);
}

if (!existsOnPath("cargo-llvm-cov")) {
  if (!installTools) {
    process.stderr.write(
      "\ncoverage: cargo-llvm-cov is not installed. Run `npm run coverage -- --install-tools` or install it with `cargo install cargo-llvm-cov --locked`.\n",
    );
    process.exit(1);
  }
  run("install cargo-llvm-cov", "cargo", ["install", "cargo-llvm-cov", "--locked"]);
}

fs.mkdirSync(path.join(coverageRoot, "engine"), { recursive: true });
const llvmEnv = llvmToolEnv();
run("engine llvm-cov lcov", "cargo", [
  "llvm-cov",
  "--workspace",
  "--lcov",
  "--output-path",
  path.join("..", "coverage", "engine", "lcov.info"),
], {
  cwd: path.join(repoRoot, "engine"),
  env: llvmEnv,
});
run("engine llvm-cov html", "cargo", [
  "llvm-cov",
  "--workspace",
  "--html",
  "--output-dir",
  path.join("..", "coverage", "engine"),
], {
  cwd: path.join(repoRoot, "engine"),
  env: llvmEnv,
});

process.stdout.write("\ncoverage reports:\n");
if (!engineOnly) {
  process.stdout.write(`  client: ${path.relative(repoRoot, path.join(repoRoot, "client", "coverage", "index.html"))}\n`);
}
process.stdout.write(`  engine: ${path.relative(repoRoot, path.join(coverageRoot, "engine", "html", "index.html"))}\n`);
process.stdout.write(`  lcov:   ${path.relative(repoRoot, path.join(coverageRoot, "engine", "lcov.info"))}\n`);
