import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { usePlayTimeStore, playSecondsFor } from "./playTime";

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
    usePlayTimeStore.setState({ byHost: {}, lastSampleMs: 0 });
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
