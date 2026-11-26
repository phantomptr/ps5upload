// What a copy check's problems mean, in a form a screen can say in plain words. The engine's
// check of a package says what kind of damage it found (`damage`); this turns that into the
// one verdict shown, and leaves the technical lines for a "Details" fold.

import type { CopyCheck } from "../api/collection";

export type CopyVerdict =
  /** Blocks of the game's data are not the bytes the package's own digests say. */
  | { kind: "data"; bad: number; total: number; percent: string; bytes: number }
  /** The file ends early; `have`/`need` are 0 when only its very end is missing. */
  | { kind: "truncated"; have: number; need: number }
  /** The image's own tables fail: the game can't be read out of it. */
  | { kind: "structure"; count: number }
  /** Only what the package says about itself fails; the game's data is right. */
  | { kind: "metadata"; count: number }
  /** Problems the check can't sort further (a folder, an archive, a file read byte by byte,
   *  or an engine that doesn't say). */
  | { kind: "other"; count: number };

/** The share of blocks that are bad, to one decimal place, never rounded to nothing or to
 *  everything: "3.7", "<0.1" for anything under a tenth of a percent, "100" only for all. */
export function percentOf(bad: number, total: number): string {
  if (total <= 0) return "100";
  const p = (bad / total) * 100;
  if (p < 0.1) return "<0.1";
  // "100" is only ever every block.
  const tenths = Math.round(p * 10) / 10;
  return String(tenths >= 100 && bad < total ? 99.9 : tenths);
}

/** The verdict for a check that found problems. */
export function copyVerdict(c: Pick<CopyCheck, "problems_total" | "damage">): CopyVerdict {
  const d = c.damage;
  switch (d?.kind) {
    case "data":
      return {
        kind: "data",
        bad: d.blocks_bad,
        total: d.blocks_total,
        percent: percentOf(d.blocks_bad, d.blocks_total),
        bytes: d.blocks_bad * d.block_bytes,
      };
    case "truncated":
      return { kind: "truncated", have: d.bytes_have, need: d.bytes_need };
    case "structure":
      return { kind: "structure", count: d.checks_failed };
    case "metadata":
      return { kind: "metadata", count: d.checks_failed };
    default:
      return { kind: "other", count: c.problems_total };
  }
}
