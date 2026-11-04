import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const context = {};
vm.createContext(context);
vm.runInContext(readFileSync(new URL("../site/platform.js", import.meta.url), "utf8"), context);
const { chooseReleases, detectPlatform, pickFiles } = context;

// vm results come from another realm, so compare plain copies.
const plain = (v) => JSON.parse(JSON.stringify(v));

const names = [
  "PS5Upload-11.0.8-mac-arm64.dmg",
  "PS5Upload-11.0.8-mac-x64.dmg",
  "PS5Upload-11.0.8-win-x64-setup.exe",
  "PS5Upload-11.0.8-win-x64.zip",
  "PS5Upload-11.0.8-win-arm64-setup.exe",
  "PS5Upload-11.0.8-win-arm64.zip",
  "PS5Upload-11.0.8-linux-x64.deb",
  "PS5Upload-11.0.8-linux-x64.rpm",
  "PS5Upload-11.0.8-linux-x64.AppImage",
  "PS5Upload-11.0.8-linux-x64.zip",
  "PS5Upload-11.0.8-linux-arm64.deb",
  "PS5Upload-11.0.8-linux-arm64.rpm",
  "PS5Upload-11.0.8-android.apk",
  "PS5Upload-11.0.8-android-armv7.apk",
  "PS5Upload-11.0.8-ios-unsigned.ipa",
].map((name) => ({ name }));
const groups = {
  mac: names.filter((f) => f.name.includes("-mac-")),
  windows: names.filter((f) => f.name.includes("-win-")),
  linux: names.filter((f) => f.name.includes("-linux-")),
  android: names.filter((f) => f.name.includes("-android")),
  ios: names.filter((f) => f.name.includes("-ios-")),
};

/** The file names offered to a browser. */
function offer(userAgent, { maxTouchPoints = 0, hints } = {}) {
  const platform = detectPlatform({ userAgent, maxTouchPoints }, hints);
  return { os: platform.os, files: plain(pickFiles(groups[platform.os], platform)).map((f) => f.name.replace("PS5Upload-11.0.8-", "")) };
}

const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const WIN = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";

test("iPadOS Safari, which says it is a Mac, gets the iPhone build", () => {
  assert.deepEqual(plain(offer(MAC, { maxTouchPoints: 5 })), { os: "ios", files: ["ios-unsigned.ipa"] });
});

test("a Mac browser that can't say which chip is offered both builds", () => {
  assert.deepEqual(plain(offer(MAC)), { os: "mac", files: ["mac-arm64.dmg", "mac-x64.dmg"] });
});

test("Chromium on a Mac says which chip through its hints", () => {
  assert.deepEqual(plain(offer(MAC, { hints: { architecture: "arm" } })).files, ["mac-arm64.dmg"]);
  assert.deepEqual(plain(offer(MAC, { hints: { architecture: "x86" } })).files, ["mac-x64.dmg"]);
});

test("Windows: x64 by default, the ARM64 installer when the browser says so", () => {
  assert.deepEqual(plain(offer(WIN)).files, ["win-x64-setup.exe"]);
  assert.deepEqual(plain(offer(WIN, { hints: { architecture: "arm" } })).files, ["win-arm64-setup.exe"]);
  assert.deepEqual(plain(offer("Mozilla/5.0 (Windows NT 10.0; ARM64) Firefox/130.0")).files, ["win-arm64-setup.exe"]);
});

test("Linux: the CPU and the distro's package type come from the user agent", () => {
  assert.deepEqual(plain(offer("Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")).files, ["linux-x64.deb"]);
  assert.deepEqual(plain(offer("Mozilla/5.0 (X11; Fedora; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0")).files, ["linux-x64.rpm"]);
  assert.deepEqual(plain(offer("Mozilla/5.0 (X11; Linux aarch64; rv:130.0) Gecko/20100101 Firefox/130.0")).files, ["linux-arm64.deb"]);
  assert.deepEqual(plain(offer("Mozilla/5.0 (X11; Fedora; Linux aarch64; rv:130.0) Gecko/20100101 Firefox/130.0")).files, ["linux-arm64.rpm"]);
});

test("phones get their one build", () => {
  assert.deepEqual(plain(offer("Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 Chrome/130 Mobile Safari/537.36")), { os: "android", files: ["android.apk"] });
  assert.deepEqual(plain(offer("Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1")), { os: "ios", files: ["ios-unsigned.ipa"] });
});

test("a release missing the best file still offers something for that CPU", () => {
  const files = [{ name: "PS5Upload-1-linux-arm64.zip" }, { name: "PS5Upload-1-linux-x64.zip" }];
  const fedora = detectPlatform({ userAgent: "Mozilla/5.0 (X11; Fedora; Linux x86_64)", maxTouchPoints: 0 });
  assert.equal(plain(pickFiles(files, fedora))[0].name, "PS5Upload-1-linux-x64.zip");
  assert.deepEqual(plain(pickFiles([], fedora)), []);
  assert.deepEqual(plain(pickFiles(undefined, fedora)), []);
});

test("the main offer is the newest stable release, not a newer pre-release", () => {
  const pre = { tag: "v2", prerelease: true };
  const stable = { tag: "v1", prerelease: false };
  const old = { tag: "v0", prerelease: false };
  let r = chooseReleases([pre, stable, old]);
  assert.equal(r.main.tag, "v1");
  assert.equal(r.pre.tag, "v2");
  r = chooseReleases([stable, old]);
  assert.equal(r.main.tag, "v1");
  assert.equal(r.pre, null);
  r = chooseReleases([pre]);
  assert.equal(r.main.tag, "v2");
  assert.equal(r.pre, null);
  r = chooseReleases([]);
  assert.equal(r.main, null);
});
