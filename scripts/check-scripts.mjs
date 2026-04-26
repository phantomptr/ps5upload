#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const skipParts = new Set([
  ".git",
  "node_modules",
  "client/node_modules",
  "target",
  "engine/target",
  "client/src-tauri/target",
  "dist",
  "client/dist",
  "coverage",
  "client/coverage",
  "bench/fixtures",
  "bench/reports",
]);

function walk(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    const rel = path.relative(repoRoot, full).replaceAll(path.sep, "/");
    if ([...skipParts].some((part) => rel === part || rel.startsWith(`${part}/`))) {
      continue;
    }
    if (entry.isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

function run(label, cmd, args) {
  const res = spawnSync(cmd, args, {
    cwd: repoRoot,
    stdio: "pipe",
    encoding: "utf8",
  });
  if (res.status !== 0) {
    process.stderr.write(`\n[check-scripts] ${label} failed\n`);
    if (res.stdout) process.stderr.write(res.stdout);
    if (res.stderr) process.stderr.write(res.stderr);
    return false;
  }
  return true;
}

function hasCommand(cmd) {
  const probe = process.platform === "win32" ? ["where", [cmd]] : ["command", ["-v", cmd]];
  return spawnSync(probe[0], probe[1], { shell: true, stdio: "ignore" }).status === 0;
}

const files = walk(repoRoot);
const nodeFiles = files.filter((f) => /\.(mjs|js)$/i.test(f));
const shellFiles = files.filter((f) => /\.sh$/i.test(f));
const pythonFiles = files.filter((f) => /\.py$/i.test(f));
const psFiles = files.filter((f) => /\.ps1$/i.test(f));

let ok = true;
for (const f of nodeFiles) ok = run(path.relative(repoRoot, f), "node", ["--check", f]) && ok;
if (hasCommand("bash")) {
  for (const f of shellFiles) ok = run(path.relative(repoRoot, f), "bash", ["-n", f]) && ok;
} else if (shellFiles.length > 0) {
  process.stdout.write("[check-scripts] bash not found; skipped shell parser checks\n");
}
if (hasCommand("python3")) {
  for (const f of pythonFiles) ok = run(path.relative(repoRoot, f), "python3", ["-m", "py_compile", f]) && ok;
} else if (pythonFiles.length > 0) {
  process.stdout.write("[check-scripts] python3 not found; skipped Python parser checks\n");
}

const pwsh = spawnSync("pwsh", ["-NoProfile", "-Command", "$PSVersionTable.PSVersion"], {
  stdio: "ignore",
});
if (pwsh.status === 0) {
  for (const f of psFiles) {
    // PowerShell's `[ref]` requires the variable to exist before being
    // passed by reference — calling `[ref]$errors` against an
    // undeclared `$errors` produces "[ref] cannot be applied to a
    // variable that does not exist" at runtime even before the parser
    // executes. Declare both `$tokens` and `$errors` up front so the
    // ParseFile output parameters land in real variables.
    const escaped = f.replaceAll("'", "''");
    const cmd = `$tokens = $null; $errors = $null; [System.Management.Automation.Language.Parser]::ParseFile('${escaped}', [ref]$tokens, [ref]$errors) > $null; if ($errors.Count) { $errors | ForEach-Object { Write-Error $_ }; exit 1 }`;
    ok = run(path.relative(repoRoot, f), "pwsh", ["-NoProfile", "-Command", cmd]) && ok;
  }
} else if (psFiles.length > 0) {
  process.stdout.write("[check-scripts] pwsh not found; skipped PowerShell parser checks\n");
}

if (!ok) process.exit(1);
process.stdout.write(
  `[check-scripts] ok (${nodeFiles.length} node, ${shellFiles.length} shell, ${pythonFiles.length} python, ${psFiles.length} powershell)\n`,
);
