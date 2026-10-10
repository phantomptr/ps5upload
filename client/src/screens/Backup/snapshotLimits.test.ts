// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { SNAPSHOT_MAX_DEPTH, SNAPSHOTS_KEPT_PER_NAME } from "./snapshotLimits";

// The screen states these limits as facts, so they must be the payload's.
const backupC: string = readFileSync(
  new URL("../../../../payload/src/backup.c", import.meta.url).pathname,
  "utf8",
);

describe("snapshot limits shown on the Console snapshots screen", () => {
  it("match the payload's keep count", () => {
    expect(backupC).toMatch(
      new RegExp(`#define\\s+BACKUPS_KEEP_PER_TAG\\s+${SNAPSHOTS_KEPT_PER_NAME}\\b`),
    );
  });

  it("match the payload's depth cut-off", () => {
    expect(backupC).toContain(`if (depth > ${SNAPSHOT_MAX_DEPTH}) return 0;`);
  });

  it("match the payload's path flattening", () => {
    expect(backupC).toContain("(*p == '/') ? '_' : *p");
  });
});
