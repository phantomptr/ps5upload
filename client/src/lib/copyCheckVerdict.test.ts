import { describe, expect, it } from "vitest";

import type { CopyDamage } from "../api/collection";
import { copyVerdict, percentOf } from "./copyCheckVerdict";

const damage = (d: Partial<CopyDamage> & { kind: string }): CopyDamage => ({
  blocks_bad: 0,
  blocks_total: 0,
  block_bytes: 0,
  bytes_have: 0,
  bytes_need: 0,
  checks_failed: 0,
  ...d,
});

describe("copyVerdict", () => {
  it("says how much of a package's data is damaged", () => {
    // The report that asked "what does this mean?": 56116 of 1529208 blocks of 64 KiB.
    const v = copyVerdict({
      problems_total: 4,
      damage: damage({ kind: "data", blocks_bad: 56116, blocks_total: 1529208, block_bytes: 65536 }),
    });
    expect(v).toEqual({ kind: "data", bad: 56116, total: 1529208, percent: "3.7", bytes: 56116 * 65536 });
  });

  it("tells a file cut short from one whose end alone is missing", () => {
    expect(
      copyVerdict({ problems_total: 1, damage: damage({ kind: "truncated", bytes_have: 500, bytes_need: 900 }) }),
    ).toEqual({ kind: "truncated", have: 500, need: 900 });
    expect(copyVerdict({ problems_total: 1, damage: damage({ kind: "truncated" }) })).toEqual({
      kind: "truncated",
      have: 0,
      need: 0,
    });
  });

  it("keeps header-only and table damage apart from damaged data", () => {
    expect(copyVerdict({ problems_total: 2, damage: damage({ kind: "metadata", checks_failed: 2 }) })).toEqual({
      kind: "metadata",
      count: 2,
    });
    expect(copyVerdict({ problems_total: 3, damage: damage({ kind: "structure", checks_failed: 1 }) })).toEqual({
      kind: "structure",
      count: 1,
    });
  });

  it("only counts the problems when the check can't sort them", () => {
    // A folder, an archive, a file read byte by byte, or an older engine.
    expect(copyVerdict({ problems_total: 7 })).toEqual({ kind: "other", count: 7 });
    expect(copyVerdict({ problems_total: 7, damage: null })).toEqual({ kind: "other", count: 7 });
    // A kind this app doesn't know yet is not guessed at.
    expect(copyVerdict({ problems_total: 2, damage: damage({ kind: "something new" }) })).toEqual({
      kind: "other",
      count: 2,
    });
  });
});

describe("percentOf", () => {
  it("never rounds damage down to nothing or up to everything", () => {
    expect(percentOf(1, 1_529_208)).toBe("<0.1");
    expect(percentOf(1529, 1_529_208)).toBe("<0.1");
    expect(percentOf(1530, 1_529_208)).toBe("0.1");
    expect(percentOf(56116, 1529208)).toBe("3.7");
    expect(percentOf(1, 2)).toBe("50");
    expect(percentOf(999, 1000)).toBe("99.9");
    expect(percentOf(9999, 10000)).toBe("99.9");
    expect(percentOf(10, 10)).toBe("100");
    expect(percentOf(3, 0)).toBe("100");
  });
});
