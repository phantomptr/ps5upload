#!/usr/bin/env node
// The local gate. Quick repo checks run first, one after another; then three independent lanes
// (engine, desktop, client) run side by side, each with its own log under output/validate/, since
// they share no build directory and the engine tests mostly sit in sleeps. A lane that fails
// prints the tail of its log; the others are left to finish so one run reports every failure.
import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const repoRoot = path.resolve(import.meta.dirname, "..");
const full = process.argv.includes("--full");
const hardware = process.argv.includes("--hardware");
const skipBuild = process.argv.includes("--skip-build");
const serial = process.argv.includes("--serial") || process.env.PS5UPLOAD_VALIDATE_SERIAL === "1";
const logDir = path.join(repoRoot, "output", "validate");
const engineDir = path.join(repoRoot, "engine");
const desktopDir = path.join(repoRoot, "client", "src-tauri");
const clientDir = path.join(repoRoot, "client");
const isWin = os.platform() === "win32";

function runInline(label, command, args, opts = {}) {
  process.stdout.write(`\n==> ${label}\n`);
  const res = spawnSync(command, args, {
    cwd: opts.cwd || repoRoot,
    stdio: "inherit",
    shell: isWin,
    env: { ...process.env, ...opts.env },
  });
  if (res.status !== 0) {
    process.stderr.write(`\nvalidate-repo: ${label} failed with exit ${res.status}\n`);
    process.exit(res.status || 1);
  }
}

const has = (cmd, args) => spawnSync(cmd, args, { stdio: "ignore", shell: isWin }).status === 0;
// nextest runs test binaries in parallel (cargo test runs them one at a time); it skips doctests,
// so those run on their own. Without nextest installed, plain cargo test, as before.
const nextest = has("cargo", ["nextest", "--version"]);

/** One lane: steps run in order, output to <logDir>/<name>.log. Resolves to null or a failure. */
function lane(name, steps) {
  const logPath = path.join(logDir, `${name}.log`);
  const log = fs.createWriteStream(logPath);
  const started = Date.now();
  return (async () => {
    for (const s of steps) {
      const t0 = Date.now();
      log.write(`\n==> ${s.label}\n`);
      const code = await new Promise((resolve) => {
        const child = spawn(s.command, s.args, {
          cwd: s.cwd || repoRoot,
          shell: isWin,
          env: { ...process.env, ...s.env },
        });
        child.stdout.on("data", (d) => log.write(d));
        child.stderr.on("data", (d) => log.write(d));
        child.on("error", (e) => {
          log.write(`spawn failed: ${e.message}\n`);
          resolve(1);
        });
        child.on("close", (c) => resolve(c ?? 1));
      });
      const secs = ((Date.now() - t0) / 1000).toFixed(0);
      if (code !== 0) {
        await new Promise((r) => log.end(r));
        return { name, label: s.label, code, logPath };
      }
      process.stdout.write(`   [${name}] ${s.label} ok (${secs}s)\n`);
    }
    await new Promise((r) => log.end(r));
    process.stdout.write(`   [${name}] lane done in ${((Date.now() - started) / 1000).toFixed(0)}s\n`);
    return null;
  })();
}

const step = (label, command, args, cwd, env) => ({ label, command, args, cwd, env });

/** CC/AR for aarch64-linux-android from the newest NDK under ANDROID_HOME or the default SDK path. */
function androidNdkEnv() {
  if (process.env.CC_aarch64_linux_android) return {};
  const sdk = process.env.ANDROID_HOME || path.join(os.homedir(), "Library", "Android", "sdk");
  const ndkRoot = path.join(sdk, "ndk");
  let versions = [];
  try {
    versions = fs.readdirSync(ndkRoot).sort((a, b) => a.localeCompare(b, undefined, { numeric: true }));
  } catch {
    return {};
  }
  const host = os.platform() === "darwin" ? "darwin-x86_64" : os.platform() === "win32" ? "windows-x86_64" : "linux-x86_64";
  const bin = path.join(ndkRoot, versions[versions.length - 1] ?? "", "toolchains", "llvm", "prebuilt", host, "bin");
  if (!fs.existsSync(bin)) return {};
  return {
    CC_aarch64_linux_android: path.join(bin, "aarch64-linux-android24-clang"),
    AR_aarch64_linux_android: path.join(bin, "llvm-ar"),
  };
}

runInline("version sync", "node", ["scripts/update-version.js", "--check"]);
runInline("script syntax", "node", ["scripts/check-scripts.mjs"]);
runInline("linux launcher", "sh", ["scripts/release/linux-launcher-selftest.sh"]);
runInline("script inventory", "node", ["scripts/audit-scripts.mjs"]);
runInline("i18n coverage", "node", ["scripts/i18n-coverage.mjs"]);
runInline("git whitespace", "git", ["diff", "--check"]);

const engineSteps = [
  step("engine fmt", "cargo", ["fmt", "--all", "--", "--check"], engineDir),
  step("engine clippy", "cargo", ["clippy", "--workspace", "--", "-D", "warnings"], engineDir),
  ...(nextest
    ? [
        step("engine tests (nextest)", "cargo", ["nextest", "run", "--workspace", "--exclude", "ava1-ctest", "--no-fail-fast"], engineDir),
        step("engine doctests", "cargo", ["test", "--workspace", "--exclude", "ava1-ctest", "--doc"], engineDir),
        // Each nextest test is its own process, so the C interop tests' process-wide C server
        // is not shared; the timing-sensitive ones are serialized by engine/.config/nextest.toml.
        step("ava1 C interop tests (nextest)", "cargo", ["nextest", "run", "-p", "ava1-ctest", "--no-fail-fast"], engineDir),
      ]
    : [
        step("engine tests", "cargo", ["test", "--workspace", "--exclude", "ava1-ctest"], engineDir),
        // The C interop tests share one C server per process and must run serially.
        step("ava1 C interop tests", "cargo", ["test", "-p", "ava1-ctest", "--", "--test-threads=1"], engineDir),
      ]),
];

// clippy --all-targets type-checks everything `cargo check --all-targets` did.
const desktopSteps = [
  step("desktop clippy", "cargo", ["clippy", "--all-targets", "--", "-D", "warnings"], desktopDir),
  step("desktop tests", "cargo", ["test"], desktopDir),
];

const clientSteps = [
  step("client typecheck", "npm", ["run", "typecheck"], clientDir),
  step("client lint", "npm", ["run", "lint"], clientDir),
  step("client tests", "npm", ["test"], clientDir),
  ...(skipBuild ? [] : [step("client vite build", "npm", ["run", "build:vite"], clientDir)]),
];

fs.mkdirSync(logDir, { recursive: true });
process.stdout.write(`\n==> lanes: engine, desktop, client${serial ? " (serial)" : " (parallel)"}; logs in ${path.relative(repoRoot, logDir)}/\n`);
if (!nextest) process.stdout.write("   (cargo-nextest not installed: engine tests run one binary at a time; `cargo install cargo-nextest --locked`)\n");

const lanes = [
  ["engine", engineSteps],
  ["desktop", desktopSteps],
  ["client", clientSteps],
];
const failures = [];
if (serial) {
  for (const [n, s] of lanes) {
    const f = await lane(n, s);
    if (f) failures.push(f);
  }
} else {
  for (const f of await Promise.all(lanes.map(([n, s]) => lane(n, s)))) if (f) failures.push(f);
}

if (failures.length) {
  for (const f of failures) {
    const text = fs.readFileSync(f.logPath, "utf8");
    const tail = text.split("\n").slice(-80).join("\n");
    process.stderr.write(`\n---- [${f.name}] ${f.label} failed with exit ${f.code} (full log: ${path.relative(repoRoot, f.logPath)}) ----\n${tail}\n`);
  }
  process.stderr.write(`\nvalidate-repo: ${failures.map((f) => `${f.name}: ${f.label}`).join("; ")} failed\n`);
  process.exit(failures[0].code || 1);
}

if (full) {
  runInline("payload ELF build/validation", "make", ["test-payload"]);
  // The rest of `make test` is the three lanes above; only its root script checks are new here.
  runInline("root script checks", "make", ["test-root"]);
  if (spawnSync("rustup", ["target", "list", "--installed"], { encoding: "utf8" }).stdout?.includes("aarch64-linux-android")) {
    // ring's C code needs the NDK's clang for this target; point at the newest NDK found.
    runInline("engine compiles for Android", "cargo", ["check", "-p", "ps5upload-engine", "--target", "aarch64-linux-android"], {
      cwd: engineDir,
      env: androidNdkEnv(),
    });
  } else {
    process.stdout.write("\n==> engine Android check — skipped (rustup target add aarch64-linux-android)\n");
  }

  // Mobile (Android) compile. The desktop and mobile builds share the command
  // layer but have separate engine modules (engine.rs vs engine_mobile.rs); a
  // helper added to one but used by a shared command compiles on the host yet
  // breaks the Android target — exactly the kind of regression CI's `android`
  // job catches and the desktop-only checks above miss. Best-effort: skipped
  // cleanly on a machine without the SDK/NDK/JDK.
  const hasRustup = has("rustup", ["--version"]);
  if (hasRustup && process.env.ANDROID_HOME) {
    runInline("android compile (mobile target)", "make", ["android-build"]);
  } else {
    process.stdout.write(
      "\n==> android compile — skipped (no Android toolchain: need rustup + ANDROID_HOME + NDK + JDK 17)\n",
    );
  }
}

if (hardware) {
  runInline("live PS5 validate", "make", ["validate"]);
}

process.stdout.write("\nvalidate-repo: all selected checks passed\n");
