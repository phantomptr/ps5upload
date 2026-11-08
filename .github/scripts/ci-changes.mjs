#!/usr/bin/env node
// Which `ci` lanes a change needs (engine-ci.yml's `changes` job, and the PR gate that checks the
// lanes it ran). Reads the changed paths, one per line, on stdin; prints `lane=true|false` lines
// for $GITHUB_OUTPUT.
//
//   git diff --name-only BASE HEAD | node .github/scripts/ci-changes.mjs
//   node .github/scripts/ci-changes.mjs --all       every lane (push to main, dispatch, schedule)
//
// The rule that keeps a skip honest: a path no rule below names runs EVERY lane, and so does an
// empty or unreadable change list. A rule is only a promise that the path cannot affect the lanes
// it leaves out; when a test starts reading a file from another part of the tree (client tests
// read engine sources, the engine embeds API.md and payload/include/config.h), the rule for that
// file must name the reader's lane too. Pushes to main run every lane, so a rule that is too
// narrow shows up there, on the next commit, rather than in a release.

/** Every lane, and the ci jobs it runs. */
export const LANES = {
  engine: "rust (engine workspace), rust tests, go live, engine targets, ava1",
  hub: "rust hub (only when the engine lane is off; that lane tests the hub too)",
  ctest: "payload C over AVA1, and its sanitized run",
  payload: "payload ELF and its host self-tests",
  client: "client (ts + vite)",
  desktop: "rust desktop (3 OS)",
  android: "android (apk build)",
  scripts: "repo scripts on macOS and Windows (Ubuntu always runs)",
};

const ALL = Object.keys(LANES);
const RUST_USERS = ["engine", "client", "desktop", "android"];

// First match wins. `lanes: ALL` is a path everything can depend on.
// Patterns: a trailing `/**` is a directory prefix, `*` matches within one path segment.
export const RULES = [
  // The pipeline itself and what every build resolves through.
  { pattern: ".github/workflows/**", lanes: ALL },
  { pattern: ".github/scripts/**", lanes: ALL },
  { pattern: ".github/actions/**", lanes: ALL },
  { pattern: "engine/Cargo.lock", lanes: ALL },
  { pattern: "engine/Cargo.toml", lanes: ALL },
  { pattern: "client/src-tauri/Cargo.lock", lanes: ALL },
  { pattern: "client/package-lock.json", lanes: ALL },
  { pattern: "client/package.json", lanes: ALL },
  { pattern: "package.json", lanes: ALL },
  { pattern: "package-lock.json", lanes: ALL },
  { pattern: "Makefile", lanes: ALL },
  { pattern: "VERSION", lanes: ALL },
  { pattern: ".gitmodules", lanes: ALL },
  { pattern: ".gitattributes", lanes: ALL },
  { pattern: ".cargo/**", lanes: ALL },
  { pattern: "third_party/**", lanes: ALL },
  // The AVA1 schema generates C, Rust and the client's codecs.
  { pattern: "protocol/**", lanes: ALL },
  // Embedded by the core crate and the desktop app, and version-checked by the engine's tests.
  { pattern: "payload/installer/**", lanes: ALL },
  { pattern: "payload/include/config.h", lanes: ALL },

  // Workflow-only metadata that no job reads.
  { pattern: ".github/ISSUE_TEMPLATE/**", lanes: [] },
  { pattern: ".github/PULL_REQUEST_TEMPLATE.md", lanes: [] },
  { pattern: ".github/CODE_REVIEW_CHECKLIST.md", lanes: [] },
  { pattern: ".github/CODEOWNERS", lanes: [] },
  { pattern: ".github/FUNDING.yml", lanes: [] },
  { pattern: ".github/dependabot.yml", lanes: [] },
  // Read by the codeql workflow only.
  { pattern: ".github/codeql/**", lanes: [] },

  // The Play Together hub stands alone: no workspace crate depends on it and it on none.
  { pattern: "engine/crates/ps5upload-hub/**", lanes: ["hub"] },
  // The C interop suite: its crate, and the two crates it builds on.
  { pattern: "engine/crates/ps5upload-ctest/**", lanes: ["engine", "ctest"] },
  { pattern: "engine/crates/ps5upload-core/**", lanes: [...RUST_USERS, "ctest"] },
  { pattern: "engine/crates/ps5upload-ava1/**", lanes: [...RUST_USERS, "ctest"] },
  // The rest of the engine workspace. The desktop and Android apps link the engine, and the
  // client's tests read engine sources and the pages the engine embeds.
  { pattern: "engine/**", lanes: RUST_USERS },
  // Embedded by the engine (its MCP resources).
  { pattern: "API.md", lanes: RUST_USERS },
  { pattern: "AUTOMATION.md", lanes: RUST_USERS },
  { pattern: "MCP.md", lanes: RUST_USERS },

  // The engine's tests read payload headers; the ctest crate compiles payload C.
  { pattern: "payload/include/**", lanes: ["payload", "ctest", "engine"] },
  // A client test reads its limits.
  { pattern: "payload/src/backup.c", lanes: ["payload", "ctest", "client"] },
  { pattern: "payload/**", lanes: ["payload", "ctest"] },
  // The tailnet core: the helper links it (payload), and so does the engine (ps5upload-tailnet).
  { pattern: "tailnet/**", lanes: ["payload", "engine", "desktop", "android"] },

  // The desktop shell; Android builds the same crate, and the client's tests read its CSP
  // (tauri.conf.json) and its commands (browserInvokeCoverage, bgWork).
  { pattern: "client/src-tauri/**", lanes: ["desktop", "android", "client"] },
  // The front end. Android bundles it; the payload embeds the console page built from it.
  { pattern: "client/**", lanes: ["client", "android", "payload"] },
  // Bundled into the app (What's new, FAQ) and read by the client's tests: FAQ.md's grammar
  // (MarkdownView.test.tsx), and the What's new screen in the browser tests
  // (e2e/bundled-docs.spec.ts looks for a version heading among the newest 20 sections).
  { pattern: "FAQ.md", lanes: ["client"] },
  { pattern: "CHANGELOG.md", lanes: ["client"] },
  // The site's guest and pad pages are the engine's, byte for byte (check-embedded-pages.mjs).
  { pattern: "site/join/**", lanes: ["client"] },
  { pattern: "site/pad/**", lanes: ["client"] },
  // The website and its scripts: version-sync runs site-i18n.mjs on every change.
  { pattern: "site/**", lanes: [] },
  { pattern: "scripts/site-*", lanes: ["scripts"] },
  { pattern: "scripts/i18n-*", lanes: ["client", "scripts"] },
  // Other scripts feed specific jobs (payload toolchain, Android patching, release checks):
  // everything, rather than a list that drifts.
  { pattern: "scripts/**", lanes: ALL },
  { pattern: "bench/**", lanes: ["scripts"] },
  { pattern: "tests/**", lanes: ["scripts"] },

  // Words only.
  { pattern: "*.md", lanes: [] },
  { pattern: "**/README.md", lanes: [] },
];

function toRegex(pattern) {
  if (pattern.startsWith("**/")) {
    const rest = pattern.slice(3).replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*");
    return new RegExp(`^(?:.*/)?${rest}$`);
  }
  if (pattern.endsWith("/**")) {
    const dir = pattern.slice(0, -3).replace(/[.+?^${}()|[\]\\]/g, "\\$&");
    return new RegExp(`^${dir}/`);
  }
  const body = pattern.replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, "[^/]*");
  return new RegExp(`^${body}$`);
}

const COMPILED = RULES.map((r) => ({ ...r, re: toRegex(r.pattern) }));

/** The rule a path falls under, or null (then it runs every lane). */
export function ruleFor(file) {
  return COMPILED.find((r) => r.re.test(file)) ?? null;
}

/**
 * Lanes for a list of changed paths. `why` names, per lane, the first path that turned it on.
 * No paths at all (an unreadable diff) is treated as "everything changed".
 */
export function classify(files) {
  const lanes = Object.fromEntries(ALL.map((l) => [l, false]));
  const why = {};
  const list = files.map((f) => f.trim()).filter(Boolean);
  const on = (lane, file) => {
    if (!lanes[lane]) why[lane] = file;
    lanes[lane] = true;
  };
  if (list.length === 0) {
    for (const l of ALL) on(l, "(no change list: run everything)");
  }
  for (const file of list) {
    // Script syntax and the LICENSE text are checked repo-wide by check-scripts.mjs.
    if (/\.(m?js|cjs|sh|py|ps1)$/i.test(file) || file === "LICENSE") on("scripts", file);
    const rule = ruleFor(file);
    const targets = rule ? rule.lanes : ALL;
    for (const l of targets) on(l, rule ? file : `${file} (no rule: run everything)`);
  }
  return { lanes, why, all: ALL.every((l) => lanes[l]) };
}

/** The `$GITHUB_OUTPUT` lines for a result. */
export function outputLines({ lanes, all }) {
  const out = ALL.map((l) => `${l}=${lanes[l]}`);
  // The hub has its own short job only when the engine lane, which tests it too, is off.
  out.push(`hub_only=${lanes.hub && !lanes.engine}`);
  out.push(`all=${all}`);
  return out;
}

async function main() {
  let files = [];
  if (!process.argv.includes("--all")) {
    let input = "";
    for await (const chunk of process.stdin) input += chunk;
    files = input.split("\n");
  }
  const result = classify(files);
  for (const line of outputLines(result)) process.stdout.write(`${line}\n`);
  for (const l of ALL) {
    process.stderr.write(`${result.lanes[l] ? "run " : "skip"}  ${l.padEnd(8)} ${result.why[l] ?? ""}\n`);
  }
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("ci-changes.mjs")) {
  await main();
}
