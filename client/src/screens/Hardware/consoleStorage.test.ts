import { describe, expect, it } from "vitest";

import type { HwStorage } from "../../api/ps5";
import { formatBytes } from "../../lib/format";
import { consoleStorageRows } from "./consoleStorage";

/** The reading of the 2 TB console in #944. */
const REPORTED: HwStorage = {
  total_bytes: 2005599387648,
  free_bytes: 182746480640,
  used_bytes: 1822852907008,
  reserved_bytes: 307929022464,
  user_total_bytes: 1995416338432,
  user_free_bytes: 175467462656,
  user_reserved_bytes: 307929022464,
  system_data_total_bytes: 8572829696,
  system_data_free_bytes: 7136804864,
  system_ex_total_bytes: 1610219520,
  system_ex_free_bytes: 142213120,
};

describe("the Console Storage card's figures (#944)", () => {
  it("shows the partition files are on, not the system partitions with it", () => {
    const rows = consoleStorageRows(REPORTED);
    expect(rows.totalBytes).toBe(REPORTED.user_total_bytes);
    // Free is what Volumes shows for /data: the same counter, in the same unit.
    expect(rows.freeBytes).toBe(REPORTED.user_free_bytes);
    expect(formatBytes(rows.freeBytes)).toBe("163 GiB");
    // Not the 182.7 GB that counted the system partitions' free space.
    expect(rows.freeBytes).toBeLessThan(REPORTED.free_bytes);
  });

  it("adds up: used and free make the total, and what is held back is part of used", () => {
    const rows = consoleStorageRows(REPORTED);
    expect(rows.usedBytes + rows.freeBytes).toBe(rows.totalBytes);
    expect(rows.heldBackBytes).toBe(307929022464);
    expect(rows.heldBackBytes).toBeLessThan(rows.usedBytes);
    // Held back is never taken from free a second time.
    expect(rows.freeBytes).toBe(175467462656);
  });

  it("a full drive shows no free space, whatever the system partitions have", () => {
    const rows = consoleStorageRows({ ...REPORTED, user_free_bytes: 0, free_bytes: 7279017984 });
    expect(rows.freeBytes).toBe(0);
    expect(rows.usedBytes).toBe(rows.totalBytes);
  });

  it("falls back to the totals when the helper sends no /user figures", () => {
    const rows = consoleStorageRows({
      ...REPORTED,
      user_total_bytes: 0,
      user_free_bytes: 0,
      user_reserved_bytes: 0,
    });
    expect(rows.totalBytes).toBe(REPORTED.total_bytes);
    expect(rows.freeBytes).toBe(REPORTED.free_bytes);
    expect(rows.heldBackBytes).toBe(REPORTED.reserved_bytes);
  });

  it("never shows impossible figures from a bad answer", () => {
    const rows = consoleStorageRows({
      ...REPORTED,
      user_total_bytes: 100,
      user_free_bytes: 500,
      user_reserved_bytes: 900,
    });
    expect(rows).toEqual({ totalBytes: 100, usedBytes: 0, heldBackBytes: 0, freeBytes: 100 });
  });
});
