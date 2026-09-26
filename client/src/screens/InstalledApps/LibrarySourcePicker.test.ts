import { describe, expect, it } from "vitest";
import { summariseSweep } from "./LibrarySourcePicker";

/** Return the fallback so assertions read as the user-visible English. */
const tr = (_k: string, _v?: Record<string, unknown>, fallback?: string) => fallback ?? _k;

const totals = (
  over: Partial<{
    added: [string, number][];
    skipped: number;
    withoutLibraries: number;
    notBackported: number;
    errors: string[];
  }> = {},
) => ({
  added: [] as [string, number][],
  skipped: 0,
  withoutLibraries: 0,
  notBackported: 0,
  errors: [] as string[],
  ...over,
});

describe("summariseSweep", () => {
  it("reports how many sets the sweep added", () => {
    const msg = summariseSweep(totals({ added: [["Alan Wake 2", 7]] }), 2, tr);
    expect(msg).toMatch(/Added 1/);
  });

  it("says no console answered rather than claiming nothing was found", () => {
    // The regression this guards. A sweep that could not reach ANY console
    // used to be indistinguishable from a console with no backported games —
    // the same confusion that made a fully-backported console report
    // "No backported games found" while the real fault was the transport.
    const msg = summariseSweep(totals({ errors: ["Pro: not reachable, skipped."] }), 0, tr);
    expect(msg).toMatch(/No console could be reached/i);
    expect(msg).not.toMatch(/no backported games/i);
  });

  it("separates 'already have everything' from 'found nothing'", () => {
    expect(summariseSweep(totals({ skipped: 34 }), 1, tr)).toMatch(/Nothing new/i);
    expect(summariseSweep(totals(), 1, tr)).toMatch(/No backported games found/i);
  });

  it("says why a console with fakelib folders still yielded nothing", () => {
    // Measured: a raw FW-11 rip carrying two leftover libraries. The scan now
    // refuses to harvest it, and reporting that as "no backported games found"
    // would read as the broken-scan bug all over again.
    const msg = summariseSweep(totals({ notBackported: 2 }), 1, tr);
    expect(msg).toMatch(/never downgraded/i);
    expect(msg).not.toMatch(/No backported games found/i);
  });

  it("prefers the added count even when other consoles contributed nothing", () => {
    // One reachable console with a new set plus one asleep must still read as
    // progress, not as an error.
    const msg = summariseSweep(
      totals({ added: [["Jak X", 7]], skipped: 12, errors: ["Phat: not reachable, skipped."] }),
      1,
      tr,
    );
    expect(msg).toMatch(/Added 1/);
  });
});
