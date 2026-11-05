import { describe, expect, it } from "vitest";

import type { OrganizeMove, OrganizePlanStatus } from "../../api/collection";
import { moveKey, planProgressText, toggleMove } from "./organizePlan";

const move = (from: string, set?: string, container = "/c"): OrganizeMove =>
  ({ container, from, to: `x/${from}`, set }) as OrganizeMove;

const tr = ((_k: string, vars: Record<string, unknown> | undefined, fallback?: string) =>
  Object.entries(vars ?? {}).reduce(
    (t, [k, v]) => t.replace(`{${k}}`, String(v)),
    fallback ?? "",
  )) as never;

describe("toggleMove", () => {
  const moves = [
    move("G_0.pkg", "G_0.pkg"),
    move("G_1.pkg", "G_0.pkg"),
    move("solo.pkg"),
    move("G_0.pkg", "G_0.pkg", "/other"),
  ];

  it("a tick stands for every part of a split set, in its own folder only", () => {
    const off = toggleMove(new Set(), moves, moves[1], false);
    expect([...off].sort()).toEqual(["/c|G_0.pkg", "/c|G_1.pkg"]);
    expect(off.has(moveKey(moves[3]))).toBe(false);
    expect(toggleMove(off, moves, moves[0], true).size).toBe(0);
  });

  it("a package on its own is only itself", () => {
    expect([...toggleMove(new Set(), moves, moves[2], false)]).toEqual(["/c|solo.pkg"]);
  });
});

describe("planProgressText", () => {
  const st = (phase: string, done = 0, total = 0) =>
    ({ phase, done, total }) as unknown as OrganizePlanStatus;

  it("says how far a preview is", () => {
    expect(planProgressText(tr, st("reading", 3, 10))).toBe("Reading items: 3 of 10");
    expect(planProgressText(tr, st("finding"))).toBe("Looking through your folders…");
    expect(planProgressText(tr, null)).toBe("Reading collection items…");
  });
});
