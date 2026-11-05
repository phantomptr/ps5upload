import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const script = join(dirname(fileURLToPath(import.meta.url)), "gen-altstore-source.py");

/** An .ipa with just the Info.plist the script reads. */
const MAKE_IPA = `
import plistlib, sys, zipfile
info = {
    "CFBundleIdentifier": "com.phantomptr.ps5upload",
    "CFBundleShortVersionString": sys.argv[2],
    "CFBundleVersion": sys.argv[2],
    "MinimumOSVersion": "15.0",
    "NSCameraUsageDescription": "Scan a QR code.",
    "NSLocalNetworkUsageDescription": "Find the PS5.",
    "NSMicrophoneUsageDescription": "Voice chat.",
    "NSPhotoLibraryAddUsageDescription": "Save a clip.",
    "UIBackgroundModes": ["audio"],
}
with zipfile.ZipFile(sys.argv[1], "w") as z:
    z.writestr("Payload/PS5Upload.app/Info.plist", plistlib.dumps(info))
`;

function withDir(fn) {
  const dir = mkdtempSync(join(tmpdir(), "gen-altstore-source-"));
  try {
    return fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function makeIpa(dir, version) {
  const ipa = join(dir, `PS5Upload-${version}-ios-unsigned.ipa`);
  execFileSync("python3", ["-I", "-c", MAKE_IPA, ipa, version], { stdio: "ignore" });
  return ipa;
}

function generate(dir, ipa, version, previous) {
  const out = join(dir, "altstore.json");
  const args = ["-I", script, "--ipa", ipa, "--tag", `v${version}`, "--repo", "phantomptr/ps5upload", "--out", out];
  if (previous) args.push("--previous", previous);
  execFileSync("python3", args, { stdio: "ignore" });
  return out;
}

const readJson = (p) => JSON.parse(readFileSync(p, "utf8"));

test("the source has what Feather and AltStore require, read from the .ipa", () => {
  withDir((dir) => {
    const source = readJson(generate(dir, makeIpa(dir, "11.1.0"), "11.1.0"));
    assert.equal(source.identifier, "com.phantomptr.ps5upload.source");
    assert.ok(source.iconURL);
    assert.equal(source.apps.length, 1);
    const [app] = source.apps;
    assert.equal(app.bundleIdentifier, "com.phantomptr.ps5upload");
    assert.ok(app.iconURL);
    assert.equal(app.marketplaceID, undefined, "Feather rejects a marketplaceID");
    const [v] = app.versions;
    assert.equal(v.version, "11.1.0");
    assert.match(v.date, /^\d{4}-\d{2}-\d{2}$/);
    assert.equal(
      v.downloadURL,
      "https://github.com/phantomptr/ps5upload/releases/download/v11.1.0/PS5Upload-11.1.0-ios-unsigned.ipa",
    );
    assert.ok(v.size > 0);
    assert.equal(v.minOSVersion, "15.0");
  });
});

test("every permission in the .ipa is declared, or AltStore warns on each install", () => {
  withDir((dir) => {
    const { appPermissions } = readJson(generate(dir, makeIpa(dir, "11.1.0"), "11.1.0")).apps[0];
    assert.deepEqual(appPermissions.entitlements, []);
    assert.deepEqual(appPermissions.privacy, {
      NSCameraUsageDescription: "Scan a QR code.",
      NSLocalNetworkUsageDescription: "Find the PS5.",
      NSMicrophoneUsageDescription: "Voice chat.",
      NSPhotoLibraryAddUsageDescription: "Save a clip.",
    });
  });
});

test("a release without an .ipa keeps the previous source instead of none", () => {
  withDir((dir) => {
    const first = generate(dir, makeIpa(dir, "11.0.8"), "11.0.8");
    const previous = join(dir, "previous.json");
    writeFileSync(previous, readFileSync(first));
    rmSync(first);
    const kept = readJson(generate(dir, join(dir, "missing.ipa"), "11.1.0", previous));
    assert.equal(kept.apps[0].versions[0].version, "11.0.8");
  });
});

test("no .ipa and nothing usable to carry forward writes nothing and does not fail", () => {
  withDir((dir) => {
    const previous = join(dir, "previous.json");
    writeFileSync(previous, "<html>Not Found</html>");
    const out = generate(dir, join(dir, "missing.ipa"), "11.1.0", previous);
    assert.equal(existsSync(out), false);
  });
});

test("older versions stay listed behind the new one, without repeats and without growing forever", () => {
  withDir((dir) => {
    const previous = join(dir, "previous.json");
    for (const version of ["11.0.1", "11.0.2", "11.0.3", "11.0.4", "11.0.5", "11.0.6", "11.0.6"]) {
      const out = generate(dir, makeIpa(dir, version), version, existsSync(previous) ? previous : undefined);
      writeFileSync(previous, readFileSync(out));
    }
    const versions = readJson(previous).apps[0].versions.map((v) => v.version);
    assert.deepEqual(versions, ["11.0.6", "11.0.5", "11.0.4", "11.0.3", "11.0.2"]);
  });
});
