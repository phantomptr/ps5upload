// scripts/ci-apt.sh's mirror switch (its --rotate hook: what a failed try does, without apt) and
// scripts/ci-apt-latest.sh (whose copy of ci-apt.sh a release build runs).
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

const here = import.meta.dirname;
const ciApt = path.join(here, "ci-apt.sh");
const latest = path.join(here, "ci-apt-latest.sh");

function sandbox() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "ci-apt-test-"));
}

/** Runs `ci-apt.sh --rotate <tryNo>` over files in a temp dir; returns {stderr, read(name)}. */
function rotate(tryNo, { arch, mirrors, sources = {}, keep = [] }, dir = sandbox()) {
  const mirrorsFile = path.join(dir, "apt-mirrors.txt");
  if (mirrors !== undefined) fs.writeFileSync(mirrorsFile, mirrors);
  // `keep`: sources files an earlier call left in `dir`.
  const names = [...keep, ...Object.keys(sources)];
  for (const name of Object.keys(sources)) {
    // The sources name the list by its real path; the test's list lives in the temp dir.
    fs.writeFileSync(path.join(dir, name), sources[name].replaceAll("@LIST@", mirrorsFile));
  }
  const res = spawnSync("bash", [ciApt, "--rotate", String(tryNo)], {
    encoding: "utf8",
    env: {
      ...process.env,
      CI_APT_ARCH: arch,
      CI_APT_MIRRORS_FILE: mirrorsFile,
      CI_APT_SOURCES_FILES: names.map((n) => path.join(dir, n)).join(" ") || path.join(dir, "none"),
    },
  });
  assert.equal(res.status, 0, res.stderr);
  return { dir, stderr: res.stderr, read: (name) => fs.readFileSync(path.join(dir, name), "utf8") };
}

// GitHub's x64 image (actions/runner-images, configure-apt-sources.sh): deb822 sources that read
// the mirror list, and the list with the cloud's mirror first.
const X64_SOURCES =
  "Types: deb\nURIs: mirror+file:@LIST@\nSuites: noble noble-updates noble-backports\nComponents: main universe\n" +
  "Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\n" +
  "Types: deb\nURIs: mirror+file:@LIST@\nSuites: noble-security\nComponents: main universe\n";
const X64_LIST =
  "http://us-east-2.ec2.archive.ubuntu.com/ubuntu/\tpriority:1\n" +
  "https://archive.ubuntu.com/ubuntu/\tpriority:2\nhttps://security.ubuntu.com/ubuntu/\tpriority:3\n";
// An arm64 image: ports.ubuntu.com named in the sources; a mirror list nothing reads may exist.
const ARM_SOURCES =
  "Types: deb\nURIs: http://ports.ubuntu.com/ubuntu-ports/\nSuites: noble noble-updates noble-backports\nComponents: main universe\n\n" +
  "Types: deb\nURIs: http://ports.ubuntu.com/ubuntu-ports/\nSuites: noble-security\nComponents: main universe\n";

test("x64 runner with the mirror list: the list becomes the next archive mirror", () => {
  const second = rotate(2, { arch: "amd64", mirrors: X64_LIST, sources: { "ubuntu.sources": X64_SOURCES } });
  assert.equal(second.read("apt-mirrors.txt"), "http://archive.ubuntu.com/ubuntu/\tpriority:1\n");
  assert.match(second.stderr, /was: http:\/\/us-east-2\.ec2\.archive\.ubuntu\.com\/ubuntu\/; list, archive/);
  // The sources themselves are untouched.
  assert.match(second.read("ubuntu.sources"), /mirror\+file:/);
  const third = rotate(3, { arch: "amd64", keep: ["ubuntu.sources"] }, second.dir);
  assert.equal(third.read("apt-mirrors.txt"), "http://mirrors.edge.kernel.org/ubuntu/\tpriority:1\n");
});

test("arm64 runner is never pointed at archive.ubuntu.com (no arm64 packages there)", () => {
  // An unread list exists, as on an image that ran the same setup script.
  const junk = "https://archive.ubuntu.com/ubuntu/\tpriority:2\n";
  const second = rotate(2, { arch: "arm64", mirrors: junk, sources: { "ubuntu.sources": ARM_SOURCES } });
  // Try 2's fallback is the mirror it is already on, so it moves to the other ports mirror.
  assert.equal(second.read("ubuntu.sources"), ARM_SOURCES.replaceAll("http://ports.ubuntu.com/ubuntu-ports/", "http://mirrors.ocf.berkeley.edu/ubuntu-ports/"));
  assert.equal(second.read("apt-mirrors.txt"), junk);
  assert.match(second.stderr, /sources, ports/);
  const third = rotate(3, { arch: "arm64", keep: ["ubuntu.sources"] }, second.dir);
  assert.equal(third.read("ubuntu.sources"), ARM_SOURCES);
  for (const run of [second, third]) {
    assert.ok(!/\/\/(archive\.ubuntu\.com|mirrors\.edge\.kernel\.org)/.test(run.read("ubuntu.sources")));
  }
});

test("arm64 runner whose sources DO read a list: the list gets a ports mirror", () => {
  const sources = ARM_SOURCES.replaceAll("http://ports.ubuntu.com/ubuntu-ports/", "mirror+file:@LIST@");
  const out = rotate(2, { arch: "arm64", mirrors: "http://azure.ports.ubuntu.com/ubuntu-ports/\tpriority:1\n", sources: { "ubuntu.sources": sources } });
  assert.equal(out.read("apt-mirrors.txt"), "http://ports.ubuntu.com/ubuntu-ports/\tpriority:1\n");
});

test("an empty or odd list falls back to the machine's architecture for the family", () => {
  const arm = rotate(2, { arch: "arm64", mirrors: "", sources: { "ubuntu.sources": X64_SOURCES } });
  assert.equal(arm.read("apt-mirrors.txt"), "http://ports.ubuntu.com/ubuntu-ports/\tpriority:1\n");
  const x64 = rotate(2, { arch: "amd64", mirrors: "", sources: { "ubuntu.sources": X64_SOURCES } });
  assert.equal(x64.read("apt-mirrors.txt"), "http://archive.ubuntu.com/ubuntu/\tpriority:1\n");
});

test("x64 runner without a mirror list: the archive URL is replaced in the sources, security and PPAs are not", () => {
  const sources =
    "# deb http://azure.archive.ubuntu.com/ubuntu/ noble main (a comment stays)\n" +
    "deb http://azure.archive.ubuntu.com/ubuntu/ noble main universe\n" +
    "deb http://azure.archive.ubuntu.com/ubuntu noble-updates main universe\n" +
    "deb http://security.ubuntu.com/ubuntu/ noble-security main\n" +
    "deb https://ppa.launchpadcontent.net/someone/tools/ubuntu noble main\n";
  const out = rotate(2, { arch: "amd64", sources: { "sources.list": sources } });
  assert.equal(
    out.read("sources.list"),
    "# deb http://azure.archive.ubuntu.com/ubuntu/ noble main (a comment stays)\n" +
      "deb http://archive.ubuntu.com/ubuntu/ noble main universe\n" +
      "deb http://archive.ubuntu.com/ubuntu/ noble-updates main universe\n" +
      "deb http://security.ubuntu.com/ubuntu/ noble-security main\n" +
      "deb https://ppa.launchpadcontent.net/someone/tools/ubuntu noble main\n",
  );
  // The third try moves off the fallback it was given.
  const third = rotate(3, { arch: "amd64", keep: ["sources.list"] }, out.dir);
  assert.match(third.read("sources.list"), /^deb http:\/\/mirrors\.edge\.kernel\.org\/ubuntu\/ noble main universe$/m);
  assert.match(third.read("sources.list"), /security\.ubuntu\.com/);
});

test("sources it cannot read are left alone, and that is not an error", () => {
  const sources = "deb http://deb.debian.org/debian bookworm main\n";
  const out = rotate(2, { arch: "amd64", sources: { "sources.list": sources } });
  assert.equal(out.read("sources.list"), sources);
  assert.match(out.stderr, /reads the same ones/);
  // No sources files at all.
  assert.match(rotate(2, { arch: "arm64" }).stderr, /reads the same ones/);
});

// ─── ci-apt-latest.sh ───

/** A bare "origin" whose main has `mainScript` as scripts/ci-apt.sh, and a clone of its v1 tag. */
function taggedClone(mainScript) {
  const dir = sandbox();
  const git = (cwd, ...args) =>
    execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", ...args], { cwd, stdio: "pipe", encoding: "utf8" });
  const src = path.join(dir, "src");
  fs.mkdirSync(path.join(src, "scripts"), { recursive: true });
  git(src, "init", "-q", "-b", "main");
  fs.copyFileSync(latest, path.join(src, "scripts", "ci-apt-latest.sh"));
  fs.writeFileSync(path.join(src, "scripts", "ci-apt.sh"), 'echo "TAG copy: $*"\n');
  git(src, "add", ".");
  git(src, "commit", "-q", "-m", "release");
  git(src, "tag", "v1");
  fs.writeFileSync(path.join(src, "scripts", "ci-apt.sh"), mainScript);
  git(src, "commit", "-q", "-am", "fix the mirror");
  const clone = path.join(dir, "clone");
  git(dir, "clone", "-q", "--depth=1", "--branch", "v1", `file://${src}`, clone);
  return { clone, src };
}

function runLatest(clone, env = {}) {
  const clean = { ...process.env, RUNNER_TEMP: path.dirname(clone), ...env };
  if (!("GITHUB_EVENT_NAME" in env)) delete clean.GITHUB_EVENT_NAME;
  return spawnSync("bash", [path.join(clone, "scripts", "ci-apt-latest.sh"), "clamav", "attr"], { encoding: "utf8", env: clean });
}

test("ci-apt-latest: a tag's build runs main's ci-apt.sh", () => {
  const { clone } = taggedClone('echo "MAIN copy: $*"\n');
  for (const event of [undefined, "workflow_dispatch", "push"]) {
    const res = runLatest(clone, event ? { GITHUB_EVENT_NAME: event } : {});
    assert.equal(res.status, 0, res.stderr);
    assert.equal(res.stdout, "MAIN copy: clamav attr\n");
    assert.match(res.stderr, /using main's ci-apt\.sh/);
  }
});

test("ci-apt-latest: a pull request or merge queue runs its own copy", () => {
  const { clone } = taggedClone('echo "MAIN copy: $*"\n');
  for (const event of ["pull_request", "merge_group"]) {
    assert.equal(runLatest(clone, { GITHUB_EVENT_NAME: event }).stdout, "TAG copy: clamav attr\n");
  }
});

test("ci-apt-latest: falls back to its own copy when main's cannot be used", () => {
  // main's copy does not parse.
  const broken = taggedClone('if then fi (\n');
  const res = runLatest(broken.clone);
  assert.equal(res.stdout, "TAG copy: clamav attr\n");
  assert.match(res.stderr, /using this checkout's copy/);
  // The branch is not there (or the fetch fails).
  const ok = taggedClone('echo "MAIN copy: $*"\n');
  assert.equal(runLatest(ok.clone, { CI_APT_LATEST_BRANCH: "no-such-branch" }).stdout, "TAG copy: clamav attr\n");
  // main's copy fails: its exit status is the wrapper's, with no second run of the tag's copy.
  const failing = taggedClone('echo "MAIN copy failed"; exit 7\n');
  const failed = runLatest(failing.clone);
  assert.equal(failed.status, 7);
  assert.equal(failed.stdout, "MAIN copy failed\n");
});
