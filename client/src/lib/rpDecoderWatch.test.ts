import { describe, expect, it } from "vitest";

import { decoderVerdict, MAX_REBUILDS, stallLimit, type DecoderWatchState } from "./rpDecoderWatch";

const base: DecoderWatchState = {
  now: 10_000,
  haveHeader: true,
  state: "configured",
  lastFedAt: 9_990,
  waitingSince: 9_000,
  rebuilds: 0,
};

describe("decoderVerdict", () => {
  it("leaves a decoder alone that gave a picture recently", () => {
    expect(decoderVerdict(base)).toBe("ok");
    expect(decoderVerdict({ ...base, waitingSince: 0 })).toBe("ok");
  });

  it("rebuilds one that gave nothing for 2 s while frames kept going in", () => {
    expect(decoderVerdict({ ...base, waitingSince: 7_900 })).toBe("rebuild");
  });

  it("rebuilds one that closed under the stream", () => {
    expect(decoderVerdict({ ...base, state: "closed", waitingSince: 9_990 })).toBe("rebuild");
  });

  it("never fires when frames stopped coming (a paused stream, a hidden page)", () => {
    expect(decoderVerdict({ ...base, waitingSince: 1_000, lastFedAt: 9_000 })).toBe("ok");
    expect(decoderVerdict({ ...base, state: "closed", lastFedAt: 0 })).toBe("ok");
  });

  it("leaves a decoder that is still being set up, and one with no header to rebuild from", () => {
    expect(decoderVerdict({ ...base, waitingSince: 1_000, state: "unconfigured" })).toBe("ok");
    expect(decoderVerdict({ ...base, waitingSince: 1_000, haveHeader: false })).toBe("ok");
  });

  it("waits longer before each further rebuild, then gives up", () => {
    expect(stallLimit(0)).toBe(2000);
    expect(stallLimit(1)).toBe(4000);
    expect(stallLimit(2)).toBe(8000);
    expect(decoderVerdict({ ...base, rebuilds: 1, waitingSince: 7_000 })).toBe("ok");
    expect(decoderVerdict({ ...base, rebuilds: 1, waitingSince: 5_900 })).toBe("rebuild");
    expect(decoderVerdict({ ...base, rebuilds: MAX_REBUILDS, waitingSince: 0, state: "closed" })).toBe("give_up");
    expect(stallLimit(5)).toBe(8000);
    // After the last rebuild it waits the longest (8 s) before giving up.
    expect(decoderVerdict({ ...base, rebuilds: MAX_REBUILDS, waitingSince: 3_000 })).toBe("ok");
    expect(decoderVerdict({ ...base, now: 30_000, lastFedAt: 29_990, rebuilds: MAX_REBUILDS, waitingSince: 1 })).toBe(
      "give_up",
    );
  });
});
