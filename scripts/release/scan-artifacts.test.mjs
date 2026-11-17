import assert from "node:assert/strict";
import { execFile, execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { buildsDone, listFiles, newArtifacts } from "./scan-artifacts.mjs";

const script = new URL("./scan-artifacts.mjs", import.meta.url).pathname;
// The end-to-end tests build artifact zips with the `zip` command; Windows runners have none (the
// scanner itself only runs on Linux, in publish).
const noZip = spawnSync("zip", ["-v"]).status === 0 ? false : "no zip command on this machine";
const sha = (buf) => createHash("sha256").update(buf).digest("hex");

test("newArtifacts: build outputs not yet seen, not expired", () => {
  const list = [
    { id: 1, name: "ps5upload-payload", expired: false },
    { id: 2, name: "ps5upload-linux-x64", expired: false },
    { id: 3, name: "clamav-scanned", expired: false },
    { id: 4, name: "ps5upload-ios", expired: true },
  ];
  assert.deepEqual(
    newArtifacts(list, new Set([1])).map((a) => a.id),
    [2],
  );
});

test("buildsDone: every build-* job finished, and there is one", () => {
  assert.equal(buildsDone([]), false);
  assert.equal(buildsDone([{ name: "virus-scan", status: "in_progress" }]), false);
  assert.equal(
    buildsDone([
      { name: "build-payload", status: "completed" },
      { name: "build-client-windows-arm64", status: "in_progress" },
      { name: "virus-scan", status: "in_progress" },
    ]),
    false,
  );
  assert.equal(
    buildsDone([
      { name: "build-payload", status: "completed" },
      { name: "build-android", status: "completed" },
      { name: "virus-scan", status: "in_progress" },
    ]),
    true,
  );
});

test("listFiles: recursive and sorted", () => {
  const dir = mkdtempSync(join(tmpdir(), "scan-list-"));
  mkdirSync(join(dir, "b"));
  writeFileSync(join(dir, "b", "x"), "x");
  writeFileSync(join(dir, "a"), "a");
  assert.deepEqual(listFiles(dir), [join(dir, "a"), join(dir, "b", "x")]);
});

// A run seen through a fake API: the payload artifact is there at once, the Windows build is
// still going; on the next poll it has finished and uploaded. A fake clamscan flags a file
// named EICAR.
function fakeRun(t, { infected = false } = {}) {
  const root = mkdtempSync(join(tmpdir(), "scan-run-"));
  const mkzip = (name, files) => {
    const src = join(root, `src-${name}`);
    mkdirSync(src, { recursive: true });
    for (const [f, body] of Object.entries(files)) writeFileSync(join(src, f), body);
    execFileSync("zip", ["-q", "-r", join(root, `${name}.zip`), "."], { cwd: src });
    return readFileSync(join(root, `${name}.zip`));
  };
  const zips = {
    1: mkzip("payload", { "ps5upload.elf": "elf bytes" }),
    2: mkzip("win", infected ? { EICAR: "bad" } : { "PS5Upload.zip": "zip bytes", "ps5upload-engine.exe": "exe" }),
  };
  let polls = 0;
  const server = createServer((req, res) => {
    const json = (o) => {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify(o));
    };
    if (req.url.includes("/jobs")) {
      polls++;
      return json({
        jobs: [
          { name: "build-payload", status: "completed" },
          { name: "build-client-windows-arm64", status: polls > 1 ? "completed" : "in_progress" },
          { name: "virus-scan", status: "in_progress" },
        ],
      });
    }
    if (req.url.includes("/runs/7/artifacts")) {
      const artifacts = [{ id: 1, name: "ps5upload-payload", expired: false }];
      if (polls > 1) artifacts.push({ id: 2, name: "ps5upload-windows-arm64", expired: false });
      return json({ artifacts });
    }
    const m = req.url.match(/\/actions\/artifacts\/(\d+)\/zip$/);
    if (m) return res.end(zips[m[1]]);
    res.statusCode = 404;
    res.end();
  });
  const bin = join(root, "bin");
  mkdirSync(bin);
  writeFileSync(
    join(bin, "clamscan"),
    '#!/bin/bash\nrc=0\nfor f in "$@"; do case "$f" in */EICAR) echo "$f: Eicar FOUND"; rc=1;; esac; done\nexit $rc\n',
  );
  chmodSync(join(bin, "clamscan"), 0o755);
  // macOS has no nproc; clamscan-parallel.sh falls back to sysctl, but keep it simple.
  writeFileSync(join(bin, "nproc"), "#!/bin/sh\necho 2\n");
  chmodSync(join(bin, "nproc"), 0o755);
  writeFileSync(join(root, "LICENSE"), "licence text");
  t.after(() => server.close());
  return new Promise((resolve) =>
    server.listen(0, "127.0.0.1", () => {
      const run = () =>
        new Promise((done) =>
          execFile(
            process.execPath,
            [script, join(root, "out"), join(root, "LICENSE")],
            {
              env: {
                ...process.env,
                PATH: `${bin}:${process.env.PATH}`,
                GITHUB_API_URL: `http://127.0.0.1:${server.address().port}`,
                GITHUB_TOKEN: "t",
                GITHUB_REPOSITORY: "o/r",
                GITHUB_RUN_ID: "7",
                RUNNER_TEMP: root,
                PS5UPLOAD_SCAN_POLL_SECONDS: "0.1",
              },
            },
            (err, stdout, stderr) => done({ code: err ? err.code : 0, stdout, stderr }),
          ),
        );
      resolve({ root, run });
    }),
  );
}

test("scans each artifact as it arrives and records what scanned clean", { skip: noZip }, async (t) => {
  const { root, run } = await fakeRun(t);
  const r = await run();
  assert.equal(r.code, 0, r.stderr + r.stdout);
  const hashes = readFileSync(join(root, "out", "scanned.sha256"), "utf8").trim().split("\n").sort();
  assert.deepEqual(hashes, ["elf bytes", "zip bytes", "exe", "licence text"].map((b) => sha(b)).sort());
  assert.match(r.stdout, /scanned clean: 4 file\(s\) from 2 artifact\(s\)/);
});

test("a hit fails and marks nothing scanned", { skip: noZip }, async (t) => {
  const { root, run } = await fakeRun(t, { infected: true });
  const r = await run();
  assert.equal(r.code, 1);
  assert.match(r.stdout + r.stderr, /FOUND/);
  assert.throws(() => readFileSync(join(root, "out", "scanned.sha256")));
});
