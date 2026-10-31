import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const script = join(dirname(fileURLToPath(import.meta.url)), "gen-updater-manifest.py");

test("every asset gets its SHA-256 beside the unchanged assets map", () => {
  const dir = mkdtempSync(join(tmpdir(), "gen-updater-manifest-"));
  try {
    const release = join(dir, "release");
    mkdirSync(release);
    const files = {
      "PS5Upload-10.2.0-mac-arm64.dmg": "mac bytes",
      "PS5Upload-10.2.0-linux-x64.deb": "deb bytes",
      "notes.txt": "not an asset",
    };
    for (const [name, body] of Object.entries(files)) writeFileSync(join(release, name), body);
    const out = join(dir, "latest.json");
    execFileSync("python3", [
      "-I", script, "--release-dir", release, "--version", "10.2.0", "--tag", "v10.2.0",
      "--repo", "phantomptr/ps5upload", "--out", out,
    ], { stdio: "ignore" });
    const manifest = JSON.parse(readFileSync(out, "utf8"));
    // The shape apps before 10.2 read: plain key -> URL.
    assert.deepEqual(manifest.assets, {
      "darwin-aarch64": "https://github.com/phantomptr/ps5upload/releases/download/v10.2.0/PS5Upload-10.2.0-mac-arm64.dmg",
      "linux-x86_64-deb": "https://github.com/phantomptr/ps5upload/releases/download/v10.2.0/PS5Upload-10.2.0-linux-x64.deb",
    });
    const sha = (s) => createHash("sha256").update(s).digest("hex");
    assert.deepEqual(manifest.sha256, {
      "darwin-aarch64": sha("mac bytes"),
      "linux-x86_64-deb": sha("deb bytes"),
    });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
