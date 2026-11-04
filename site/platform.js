// Which release to offer and which of its files suits this visitor. Plain functions with no page
// access, so scripts/site-platform.test.mjs can run them; app.js calls them.
"use strict";

/** The release to offer first: the newest stable one (pre-releases are still being checked on
 *  real hardware), and the newer pre-release when there is one. */
function chooseReleases(list) {
  const main = list.find((r) => !r.prerelease) ?? list[0] ?? null;
  const pre = main && list[0] && list[0] !== main ? list[0] : null;
  return { main, pre };
}

/** OS, CPU and package type from what the browser says. `arch` is null when it can't be known:
 *  every Mac browser says "Intel" in its user agent, and Safari and Firefox have no other way.
 *  `hints` is the result of userAgentData.getHighEntropyValues (Chromium only). */
function detectPlatform(nav, hints) {
  const ua = nav.userAgent || "";
  let os = null;
  if (/android/i.test(ua)) os = "android";
  else if (/iphone|ipad|ipod/i.test(ua)) os = "ios";
  // iPadOS Safari presents itself as a Mac; a Mac has no touch screen.
  else if (/macintosh/i.test(ua) && nav.maxTouchPoints > 1) os = "ios";
  else if (/windows/i.test(ua)) os = "windows";
  else if (/mac os x|macintosh/i.test(ua)) os = "mac";
  else if (/linux|x11/i.test(ua)) os = "linux";

  let arch = null;
  if (hints?.architecture === "arm") arch = "arm64";
  else if (hints?.architecture === "x86") arch = "x64";
  else if (/\b(aarch64|arm64)\b/i.test(ua)) arch = "arm64";
  else if (os !== "mac" && /\b(x86_64|x64|amd64|wow64)\b/i.test(ua)) arch = "x64";

  return { os, arch, rpm: /fedora|opensuse|suse|red hat|rhel|centos|rocky|almalinux/i.test(ua) };
}

/** The files to offer for this platform, from its group in the release's files (each has a
 *  `name`), best first. More than one means the visitor has to choose. */
function pickFiles(files, platform) {
  if (!files?.length) return [];
  const named = (re) => files.filter((f) => re.test(f.name));
  const { os, arch, rpm } = platform;
  if (os === "mac") {
    if (!arch) return files;
    const mine = named(new RegExp(`-mac-${arch}\\.dmg$`));
    return mine.length ? mine : files;
  }
  if (os === "windows" || os === "linux") {
    // x64 builds run on most of what doesn't say, and under emulation on Windows on ARM.
    const a = arch ?? "x64";
    const ext = os === "windows" ? "setup\\.exe" : rpm ? "rpm" : "deb";
    const sep = os === "windows" ? "-" : "\\.";
    const best = named(new RegExp(`-${a}${sep}${ext}$`));
    if (best.length) return best;
    const sameArch = named(new RegExp(`-${a}[-.]`));
    return sameArch.length ? sameArch.slice(0, 1) : files.slice(0, 1);
  }
  return files.slice(0, 1);
}
