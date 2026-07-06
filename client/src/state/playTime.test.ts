import { afterEach, beforeEach, describe, expect, it } from "vitest";
import {
  usePlayTimeStore,
  playSecondsFor,
  lastSeenPlayingFor,
  daysSinceLastSeen,
  formatLastSeen,
  formatPlayTime,
} from "./playTime";

/**
 * Play-time accumulation is partitioned PER-HOST (multi-console): the same
 * title_id played on two PS5s tracks separately. These tests pin that the
 * credit/read/reset paths key by console and never bleed across them.
 *
 * The host key is port-stripped (hostOf), so `1.2.3.4` and `1.2.3.4:9113`
 * address the same bucket.
 */
describe("usePlayTimeStore (per-host)", () => {
  const reset = () =>
    usePlayTimeStore.setState({
      byHost: {},
      lastSeenByHost: {},
      lastSampleMs: 0,
    });
  beforeEach(reset);
  afterEach(reset);

  it("starts empty", () => {
    const s = usePlayTimeStore.getState();
    expect(s.byHost).toEqual({});
    expect(playSecondsFor(s, "1.2.3.4", "CUSA00001")).toBeUndefined();
  });

  it("credits a title under the console it ran on", () => {
    usePlayTimeStore.getState().credit("1.2.3.4", ["CUSA00001"], 60);
    const s = usePlayTimeStore.getState();
    expect(playSecondsFor(s, "1.2.3.4", "CUSA00001")).toBe(60);
  });

  it("keeps the SAME title separate across two consoles", () => {
    const st = usePlayTimeStore.getState();
    st.credit("1.2.3.4", ["CUSA00001"], 120); // Pro: 2 min
    st.credit("5.6.7.8", ["CUSA00001"], 30); // Phat: 30 s
    const s = usePlayTimeStore.getState();
    expect(playSecondsFor(s, "1.2.3.4", "CUSA00001")).toBe(120);
    expect(playSecondsFor(s, "5.6.7.8", "CUSA00001")).toBe(30);
    // Never merged into a single global count.
    expect(s.byHost["1.2.3.4"]["CUSA00001"]).toBe(120);
    expect(s.byHost["5.6.7.8"]["CUSA00001"]).toBe(30);
  });

  it("accumulates repeated credits on one console", () => {
    const st = usePlayTimeStore.getState();
    st.credit("1.2.3.4", ["CUSA00001"], 60);
    st.credit("1.2.3.4", ["CUSA00001"], 90);
    expect(
      playSecondsFor(usePlayTimeStore.getState(), "1.2.3.4", "CUSA00001"),
    ).toBe(150);
  });

  it("treats host:port and bare host as the same console", () => {
    const st = usePlayTimeStore.getState();
    st.credit("1.2.3.4", ["CUSA00001"], 60);
    st.credit("1.2.3.4:9113", ["CUSA00001"], 40);
    expect(
      playSecondsFor(usePlayTimeStore.getState(), "1.2.3.4:9999", "CUSA00001"),
    ).toBe(100);
  });

  it("reset only clears the target console's title", () => {
    const st = usePlayTimeStore.getState();
    st.credit("1.2.3.4", ["CUSA00001"], 100);
    st.credit("5.6.7.8", ["CUSA00001"], 50);
    st.reset("1.2.3.4", "CUSA00001");
    const s = usePlayTimeStore.getState();
    expect(playSecondsFor(s, "1.2.3.4", "CUSA00001")).toBeUndefined();
    expect(playSecondsFor(s, "5.6.7.8", "CUSA00001")).toBe(50);
  });

  it("resetAll wipes every console", () => {
    const st = usePlayTimeStore.getState();
    st.credit("1.2.3.4", ["CUSA00001"], 100);
    st.credit("5.6.7.8", ["CUSA00002"], 50);
    st.resetAll();
    expect(usePlayTimeStore.getState().byHost).toEqual({});
  });

  it("ignores empty title lists and non-positive deltas", () => {
    const st = usePlayTimeStore.getState();
    st.credit("1.2.3.4", [], 60);
    st.credit("1.2.3.4", ["CUSA00001"], 0);
    st.credit("1.2.3.4", ["CUSA00001"], -5);
    expect(usePlayTimeStore.getState().byHost).toEqual({});
  });
});

describe("last-seen-playing (#116)", () => {
  const reset = () =>
    usePlayTimeStore.setState({
      byHost: {},
      lastSeenByHost: {},
      lastSampleMs: 0,
    });
  beforeEach(reset);
  afterEach(reset);

  it("credit() stamps a last-seen timestamp for each running title", () => {
    const before = Date.now();
    usePlayTimeStore.getState().credit("1.2.3.4", ["CUSA00001"], 60);
    const s = usePlayTimeStore.getState();
    const seen = lastSeenPlayingFor(s, "1.2.3.4", "CUSA00001");
    expect(seen).toBeGreaterThanOrEqual(before);
  });

  it("returns undefined for a never-seen title (the removal candidate signal)", () => {
    usePlayTimeStore.getState().credit("1.2.3.4", ["CUSA00001"], 60);
    const s = usePlayTimeStore.getState();
    expect(lastSeenPlayingFor(s, "1.2.3.4", "CUSA99999")).toBeUndefined();
  });

  it("keeps last-seen partitioned per console", () => {
    usePlayTimeStore.getState().credit("1.1.1.1", ["CUSA00001"], 30);
    const s = usePlayTimeStore.getState();
    expect(lastSeenPlayingFor(s, "1.1.1.1", "CUSA00001")).toBeGreaterThan(0);
    expect(lastSeenPlayingFor(s, "2.2.2.2", "CUSA00001")).toBeUndefined();
  });

  it("reset() clears both seconds and last-seen for the title", () => {
    usePlayTimeStore.getState().credit("1.2.3.4", ["CUSA00001"], 60);
    usePlayTimeStore.getState().reset("1.2.3.4", "CUSA00001");
    const s = usePlayTimeStore.getState();
    expect(playSecondsFor(s, "1.2.3.4", "CUSA00001")).toBeUndefined();
    expect(lastSeenPlayingFor(s, "1.2.3.4", "CUSA00001")).toBeUndefined();
  });
});

describe("daysSinceLastSeen", () => {
  const now = 1_000_000_000_000;
  const DAY = 24 * 60 * 60 * 1000;
  it("null for undefined / zero (never seen)", () => {
    expect(daysSinceLastSeen(undefined, now)).toBeNull();
    expect(daysSinceLastSeen(0, now)).toBeNull();
  });
  it("0 for today, 1 for yesterday, N for N days", () => {
    expect(daysSinceLastSeen(now, now)).toBe(0);
    expect(daysSinceLastSeen(now - DAY, now)).toBe(1);
    expect(daysSinceLastSeen(now - 10 * DAY, now)).toBe(10);
  });
  it("never negative (clock skew / future ts)", () => {
    expect(daysSinceLastSeen(now + DAY, now)).toBe(0);
  });
});

describe("formatLastSeen", () => {
  const now = 1_000_000_000_000;
  const DAY = 24 * 60 * 60 * 1000;
  it("labels never / today / yesterday / N days ago", () => {
    expect(formatLastSeen(undefined, now)).toBe("never");
    expect(formatLastSeen(now, now)).toBe("today");
    expect(formatLastSeen(now - DAY, now)).toBe("yesterday");
    expect(formatLastSeen(now - 5 * DAY, now)).toBe("5 days ago");
  });
});

describe("formatPlayTime", () => {
  it("formats hours/minutes/seconds and — for zero", () => {
    expect(formatPlayTime(undefined)).toBe("—");
    expect(formatPlayTime(0)).toBe("—");
    expect(formatPlayTime(45)).toBe("45s");
    expect(formatPlayTime(90)).toBe("1m 30s");
    expect(formatPlayTime(3661)).toBe("1h 1m");
  });
});
