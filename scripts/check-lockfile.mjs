#!/usr/bin/env node
/**
 * Guard the client lockfile against a silent downgrade.
 *
 * `client/package-lock.json` carries `libc` fields on the platform-specific
 * optional dependencies. They are not decoration: `engine/Dockerfile.webui`
 * builds the web UI on `node:26-alpine` — musl, not glibc — and npm uses
 * `libc` to pick the matching native binaries. npm 11 writes and honours the
 * field; npm 10 does not know it and DELETES it from the lockfile on any
 * `npm install`.
 *
 * The Makefile installs client deps on every build, so a maintainer on an
 * older npm silently stripped that metadata every time, and it had to be
 * hand-reverted before each release. This check fails instead, loudly, before
 * the stripped lockfile can be committed.
 *
 * If this fires: do not "fix" it by committing the stripped file. Restore it
 *   git checkout -- client/package-lock.json
 * and use npm 11+ (Node 24+) if you need to change dependencies.
 */
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const lockPath = join(root, "client", "package-lock.json");

/** Count `libc` keys in the lockfile's package entries. */
export function countLibcEntries(lockJson) {
  const packages = lockJson?.packages ?? {};
  let n = 0;
  for (const entry of Object.values(packages)) {
    if (entry && Object.prototype.hasOwnProperty.call(entry, "libc")) n += 1;
  }
  return n;
}

/** Minimum expected. The committed lockfile has 6; a drop to zero is the
 *  signature of an old npm having rewritten it. Kept as a floor rather than an
 *  exact match so adding a dependency does not fail the build. */
export const MIN_LIBC_ENTRIES = 1;

function main() {
  let lock;
  try {
    lock = JSON.parse(readFileSync(lockPath, "utf8"));
  } catch (e) {
    console.error(`[check-lockfile] cannot read ${lockPath}: ${e.message}`);
    process.exit(1);
  }
  const n = countLibcEntries(lock);
  if (n < MIN_LIBC_ENTRIES) {
    console.error(
      "[check-lockfile] client/package-lock.json has lost its `libc` fields.\n" +
        "  These select musl vs glibc native binaries for the Alpine-based\n" +
        "  web UI Docker build (engine/Dockerfile.webui, node:26-alpine).\n" +
        "  An npm older than 11 deletes them on `npm install`.\n" +
        `  Local npm: ${process.env.npm_config_user_agent ?? "unknown"}\n` +
        "  Restore with:  git checkout -- client/package-lock.json\n" +
        "  To change dependencies, use npm 11+ (Node 24+).",
    );
    process.exit(1);
  }
  console.log(`[check-lockfile] ok (${n} libc entries preserved)`);
}

if (process.argv[1] && process.argv[1].endsWith("check-lockfile.mjs")) {
  if (process.argv.includes("--self-test")) {
    const cases = [
      [{ packages: { a: { libc: ["musl"] } } }, 1],
      [{ packages: { a: {}, b: { libc: ["glibc"] }, c: { libc: ["musl"] } } }, 2],
      [{ packages: { a: {}, b: {} } }, 0],
      [{}, 0],
      [{ packages: {} }, 0],
    ];
    let bad = 0;
    for (const [input, want] of cases) {
      const got = countLibcEntries(input);
      if (got !== want) {
        console.error(`FAIL: got ${got} want ${want} for ${JSON.stringify(input)}`);
        bad += 1;
      }
    }
    if (bad) process.exit(1);
    console.log(`✓ check-lockfile self-tests passed (${cases.length})`);
  } else {
    main();
  }
}
