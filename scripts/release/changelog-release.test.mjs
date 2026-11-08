import assert from "node:assert/strict";
import test from "node:test";
import { release, section } from "./changelog-release.mjs";

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
