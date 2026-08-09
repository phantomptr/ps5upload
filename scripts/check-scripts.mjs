#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
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
  if (process.platform === "win32") {
    return spawnSync("where", [cmd], { stdio: "ignore" }).status === 0;
  }
  return spawnSync("sh", ["-c", "command -v \"$1\" >/dev/null 2>&1", "sh", cmd], {
    stdio: "ignore",
  }).status === 0;
}

function checkSdkPin() {
  const metadataPath = path.join(repoRoot, "scripts", "ps5-sdk.env");
  const metadataText = fs.readFileSync(metadataPath, "utf8");
  const metadata = Object.fromEntries(
    metadataText
      .split(/\r?\n/)
      .filter((line) => /^[A-Z0-9_]+=/.test(line))
      .map((line) => {
        const split = line.indexOf("=");
        return [line.slice(0, split), line.slice(split + 1)];
      }),
  );
  const failures = [];
  if (!/^v\d+\.\d+(?:\.\d+)?$/.test(metadata.PS5_SDK_TAG || "")) {
    failures.push("PS5_SDK_TAG must be a v-prefixed release number");
  }
  for (const key of ["PS5_SDK_SHA256", "PS5_SDK_NID_SHA256"]) {
    if (!/^[0-9a-f]{64}$/.test(metadata[key] || "")) {
      failures.push(`${key} must be a lowercase SHA-256`);
    }
  }

  const nidPath = path.join(repoRoot, "scripts", "ps5-sdk", "prospero-nid");
  const nidHash = createHash("sha256").update(fs.readFileSync(nidPath)).digest("hex");
  if (nidHash !== metadata.PS5_SDK_NID_SHA256) {
    failures.push("portable prospero-nid does not match PS5_SDK_NID_SHA256");
  }

  const requiredReferences = new Map([
    ["scripts/install-macos.sh", "scripts/install-ps5-sdk.sh"],
    ["scripts/install-ubuntu.sh", "scripts/install-ps5-sdk.sh"],
    ["scripts/install-windows.ps1", "ps5-sdk.env"],
    [".github/workflows/engine-ci.yml", "scripts/install-ps5-sdk.sh"],
    [".github/workflows/publish.yml", "scripts/install-ps5-sdk.sh"],
  ]);
  for (const [file, needle] of requiredReferences) {
    const content = fs.readFileSync(path.join(repoRoot, file), "utf8");
    if (!content.includes(needle)) failures.push(`${file} does not use ${needle}`);
    if (content.includes("PS5_SDK_TAG: v0.")) failures.push(`${file} contains a second hard-coded SDK pin`);
  }

  const currentFiles = [
    "Makefile",
    "README.md",
    "FAQ.md",
    ...requiredReferences.keys(),
  ];
  for (const file of currentFiles) {
    const content = fs.readFileSync(path.join(repoRoot, file), "utf8");
    if (content.includes("SDK v0.41") || content.includes("SDK_TAG=\"v0.41\"")) {
      failures.push(`${file} still references the previous SDK v0.41 pin`);
    }
  }
  for (const file of ["README.md", "FAQ.md"]) {
    const content = fs.readFileSync(path.join(repoRoot, file), "utf8");
    if (!content.includes(`SDK ${metadata.PS5_SDK_TAG}`)) {
      failures.push(`${file} does not document the pinned SDK ${metadata.PS5_SDK_TAG}`);
    }
  }

  const publishWorkflow = fs.readFileSync(
    path.join(repoRoot, ".github", "workflows", "publish.yml"),
    "utf8",
  );
  const dpiArtifact = "payload/dpi/ezremote-dpi.elf.gz";
  const dpiReferences = publishWorkflow.split(dpiArtifact).length - 1;
  if (dpiReferences < 3) {
    failures.push(
      "publish.yml must build, upload, and verify the DPI installer gzip for release clients",
    );
  }

  if (failures.length > 0) {
    process.stderr.write("\n[check-scripts] PS5 SDK pin validation failed\n");
    for (const failure of failures) process.stderr.write(`  - ${failure}\n`);
    return false;
  }
  process.stdout.write(
    `[check-scripts] PS5 SDK pin ok (${metadata.PS5_SDK_TAG}, ${metadata.PS5_SDK_SHA256})\n`,
  );
  return true;
}

const files = walk(repoRoot);
const nodeFiles = files.filter((f) => /\.(mjs|js)$/i.test(f));
const shellFiles = files.filter((f) => /\.sh$/i.test(f));
const pythonFiles = files.filter((f) => /\.py$/i.test(f));
const psFiles = files.filter((f) => /\.ps1$/i.test(f));

let ok = true;
ok = checkSdkPin() && ok;
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
