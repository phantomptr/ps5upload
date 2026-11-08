import assert from "node:assert/strict";
import test from "node:test";
import { classify, outputLines, LANES } from "./ci-changes.mjs";
import { JOB_LANES } from "./pr-gate.mjs";

const on = (files) =>
  Object.entries(classify(files).lanes)
    .filter(([, v]) => v)
    .map(([k]) => k)
    .sort();
const ALL = Object.keys(LANES).sort();

test("docs only: no lane runs (version-sync, security scan and repo scripts on Ubuntu still do)", () => {
  assert.deepEqual(
    on(["README.md", "CONTRIBUTING.md", ".github/ISSUE_TEMPLATE/bug.yml", "site/index.html", "site/i18n/de.json"]),
    [],
  );
});

test("CHANGELOG.md and FAQ.md: the client job, whose tests read them", () => {
  assert.deepEqual(on(["CHANGELOG.md"]), ["client"]);
  assert.deepEqual(on(["FAQ.md"]), ["client"]);
});

test("client only: client, android, and the payload that embeds the console page", () => {
  assert.deepEqual(on(["client/src/screens/Home.tsx", "client/src/i18n/en.ts"]), ["android", "client", "payload"]);
});

test("desktop shell only: desktop, android, and the client tests that read it", () => {
  assert.deepEqual(on(["client/src-tauri/src/commands/agent_mcp.rs"]), ["android", "client", "desktop"]);
});

test("engine only: the engine and everything that links it or reads its sources", () => {
  assert.deepEqual(on(["engine/crates/ps5upload-engine/src/join_net/mod.rs"]), [
    "android",
    "client",
    "desktop",
    "engine",
  ]);
});

test("core and ava1 crates also run the C interop suite", () => {
  assert.deepEqual(on(["engine/crates/ps5upload-core/src/hw.rs"]), ["android", "client", "ctest", "desktop", "engine"]);
});

test("hub only: its own job, nothing else", () => {
  const r = classify(["engine/crates/ps5upload-hub/src/lib.rs"]);
  assert.deepEqual(on(["engine/crates/ps5upload-hub/src/lib.rs"]), ["hub"]);
  assert.ok(outputLines(r).includes("hub_only=true"));
});

test("hub plus engine: the engine lane tests the hub, so hub_only is off", () => {
  const r = classify(["engine/crates/ps5upload-hub/src/lib.rs", "engine/crates/ps5upload-engine/src/lib.rs"]);
  assert.ok(outputLines(r).includes("hub_only=false"));
});

test("payload only: payload and ctest", () => {
  assert.deepEqual(on(["payload/src/mgmt_fs.c"]), ["ctest", "payload"]);
});

test("payload headers reach the engine's tests; backup.c a client test", () => {
  assert.deepEqual(on(["payload/include/cross_device.h"]), ["ctest", "engine", "payload"]);
  assert.deepEqual(on(["payload/src/backup.c"]), ["client", "ctest", "payload"]);
});

test("tailnet: the helper and the engine", () => {
  assert.deepEqual(on(["tailnet/src/noise.c"]), ["android", "desktop", "engine", "payload"]);
});

test("a workflow, a lockfile, VERSION or a submodule runs everything", () => {
  for (const f of [
    ".github/workflows/engine-ci.yml",
    ".github/scripts/ci-changes.mjs",
    "engine/Cargo.lock",
    "client/package-lock.json",
    "client/src-tauri/Cargo.lock",
    "VERSION",
    "third_party/ava1",
    "protocol/ps5upload-ava1/app.toml",
    "payload/installer/ps5upload-installer.elf",
  ]) {
    assert.deepEqual(on([f]), ALL, f);
    assert.ok(outputLines(classify([f])).includes("all=true"), f);
  }
});

test("a path no rule names runs everything", () => {
  assert.deepEqual(on(["branding/new-icon.svg"]), ALL);
  assert.deepEqual(on(["LICENSES/new.md"]), ALL);
});

test("an empty or unreadable change list runs everything", () => {
  assert.deepEqual(on([]), ALL);
  assert.deepEqual(on(["", "  "]), ALL);
});

test("markdown the engine or the app embeds is not docs-only", () => {
  assert.ok(on(["API.md"]).includes("engine"));
  assert.ok(on(["AUTOMATION.md"]).includes("engine"));
});

test("the site's guest and pad pages are checked against the client build", () => {
  assert.deepEqual(on(["site/join/index.html"]), ["client"]);
});

test("a script anywhere gets the full repo-scripts matrix", () => {
  assert.ok(on(["site/assets/app.js"]).includes("scripts"));
  assert.ok(on(["client/scripts/foo.mjs"]).includes("scripts"));
  assert.deepEqual(on(["scripts/site-faq.mjs"]), ["scripts"]);
});

test("every lane the PR gate knows is a lane this script outputs", () => {
  const outputs = new Set(outputLines(classify([])).map((l) => l.split("=")[0]));
  for (const [job, lane] of Object.entries(JOB_LANES)) {
    if (lane !== "always") assert.ok(outputs.has(lane), `${job}: ${lane}`);
  }
});
