import assert from "node:assert/strict";
import test from "node:test";
import { main, parseArgs, release, section, USAGE } from "./changelog-release.mjs";

const head = "# Changelog\n\nWhat's new in ps5upload, written for humans.\n\n---\n\n";
const old = "## 12.0.0\n\n- Tailscale, built in.\n\n---\n\n## 11.4.0\n\n- Older.\n";

test("renames a filled Unreleased section", () => {
  const md = `${head}## Unreleased\n\n- New thing.\n\n---\n\n${old}`;
  const { md: out, changed } = release(md, "12.1.0", "12.0.0");
  assert.equal(changed, true);
  assert.equal(section(out, "12.1.0").trim(), "- New thing.");
  assert.equal(section(out, "Unreleased"), null);
  assert.equal(section(out, "12.0.0").trim(), "- Tailscale, built in.");
});

test("an existing filled version section is left alone", () => {
  const md = `${head}## 12.1.0\n\n- New thing.\n\n---\n\n${old}`;
  assert.equal(release(md, "12.1.0", "12.0.0").changed, false);
});

test("no section and no Unreleased text: refused", () => {
  assert.throws(() => release(`${head}${old}`, "12.1.0"), /no '## 12.1.0' section/);
  assert.throws(() => release(`${head}## Unreleased\n\n---\n\n${old}`, "12.1.0"), /no '## Unreleased' section with text/);
});

test("an empty version section is refused", () => {
  assert.throws(() => release(`${head}## 12.1.0\n\n---\n\n${old}`, "12.1.0"), /is empty/);
});

test("main that has not caught up with the last release is refused", () => {
  // v12.1.0 shipped from a tag; main still says Unreleased over its text.
  const md = `${head}## Unreleased\n\n- What 12.1.0 shipped.\n\n---\n\n${old}`;
  assert.throws(() => release(md, "12.2.0", "12.1.0"), /has not caught up with v12.1.0/);
});

test("a bad version is refused", () => {
  assert.throws(() => release(`${head}${old}`, "12.1"), /not X.Y.Z/);
  assert.throws(() => release(`${head}${old}`, "12.1.0", "v12.0.0"), /--previous/);
});

test("the real CHANGELOG.md has the shape this expects", async () => {
  const fs = await import("node:fs");
  const md = fs.readFileSync(new URL("../../CHANGELOG.md", import.meta.url), "utf8");
  assert.match(md, /^## \d+\.\d+\.\d+$/m);
});

// ─── The command: --check never writes ───

function fakeIo(md) {
  const writes = [];
  return { writes, read: () => md, write: (file, text) => writes.push({ file, text }) };
}
const ready = `${head}## Unreleased\n\n- New thing.\n\n---\n\n${old}`;

test("--check on a ready CHANGELOG: exit 0, nothing written", () => {
  const io = fakeIo(ready);
  const res = main(["12.1.0", "--previous", "12.0.0", "--check"], "CHANGELOG.md", io);
  assert.equal(res.code, 0);
  assert.equal(io.writes.length, 0);
  assert.match(res.out, /nothing written/);
  // The flag's position does not matter.
  const io2 = fakeIo(ready);
  assert.equal(main(["--check", "12.1.0"], "CHANGELOG.md", io2).code, 0);
  assert.equal(io2.writes.length, 0);
});

test("--check reports the same refusals as the real run, and writes nothing", () => {
  const io = fakeIo(ready);
  const res = main(["12.2.0", "--previous", "12.1.0", "--check"], "CHANGELOG.md", io);
  assert.equal(res.code, 1);
  assert.match(res.err, /has not caught up with v12.1.0/);
  assert.equal(io.writes.length, 0);
});

test("without --check the file is rewritten, and the output says so", () => {
  const io = fakeIo(ready);
  const res = main(["12.1.0", "--previous", "12.0.0"], "CHANGELOG.md", io);
  assert.equal(res.code, 0);
  assert.equal(io.writes.length, 1);
  assert.equal(section(io.writes[0].text, "12.1.0").trim(), "- New thing.");
  assert.match(res.out, /REWRITTEN/);
});

test("an already named section is not written either way", () => {
  const md = `${head}## 12.1.0\n\n- New thing.\n\n---\n\n${old}`;
  for (const args of [["12.1.0"], ["12.1.0", "--check"]]) {
    const io = fakeIo(md);
    assert.equal(main(args, "CHANGELOG.md", io).code, 0);
    assert.equal(io.writes.length, 0);
  }
});

test("bad arguments: exit 2 with the usage, nothing read or written", () => {
  for (const args of [[], ["--chekc", "12.1.0"], ["12.1.0", "12.2.0"], ["12.1.0", "--previous"], ["--dry-run", "12.1.0"]]) {
    const io = { read: () => assert.fail("read"), write: () => assert.fail("write") };
    const res = main(args, "CHANGELOG.md", io);
    assert.equal(res.code, 2, JSON.stringify(args));
    assert.match(res.err, /REWRITES CHANGELOG.md/);
  }
});

test("--help prints the usage, which names the write and --check", () => {
  const io = { read: () => assert.fail("read"), write: () => assert.fail("write") };
  const res = main(["--help"], "CHANGELOG.md", io);
  assert.equal(res.code, 0);
  assert.equal(res.out, USAGE);
  assert.match(USAGE, /REWRITES CHANGELOG\.md/);
  assert.match(USAGE, /--check/);
});

test("an empty --previous means no earlier release (release-tag passes it for the first one)", () => {
  assert.deepEqual(parseArgs(["12.1.0", "--previous", ""]), { version: "12.1.0", previous: "", check: false, help: false });
  const io = fakeIo(ready);
  assert.equal(main(["12.1.0", "--previous", "", "--check"], "CHANGELOG.md", io).code, 0);
});

test("--check on the real CHANGELOG.md leaves the file byte-identical", async () => {
  const fs = await import("node:fs");
  const { execFileSync } = await import("node:child_process");
  const file = new URL("../../CHANGELOG.md", import.meta.url);
  const before = fs.readFileSync(file);
  const script = new URL("./changelog-release.mjs", import.meta.url).pathname;
  try {
    execFileSync("node", [script, "999.0.0", "--check"], { stdio: "pipe" });
  } catch {
    // Not ready is fine here (no Unreleased text); the point is the file.
  }
  assert.ok(before.equals(fs.readFileSync(file)));
});
