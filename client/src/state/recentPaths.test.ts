import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { useRecentPathsStore } from "./recentPaths";

/**
 * Recent-paths MRU is move-to-front + capped + per-store-singleton.
 * The localStorage interactions are stubbed via direct store reset
 * so tests stay independent of each other.
 */
describe("useRecentPathsStore", () => {
  beforeEach(() => {
    if (typeof window !== "undefined") {
      window.localStorage.removeItem("ps5upload.recent_paths.v1");
    }
    useRecentPathsStore.setState({ paths: [] });
  });

  afterEach(() => {
    useRecentPathsStore.setState({ paths: [] });
  });

  it("starts empty", () => {
    expect(useRecentPathsStore.getState().paths).toEqual([]);
  });

  it("ignores the root path '/'", () => {
    useRecentPathsStore.getState().push("/");
    expect(useRecentPathsStore.getState().paths).toEqual([]);
  });

  it("ignores empty/whitespace paths", () => {
    useRecentPathsStore.getState().push("");
    useRecentPathsStore.getState().push("   ");
    expect(useRecentPathsStore.getState().paths).toEqual([]);
  });

  it("prepends new paths (MRU order)", () => {
    const s = useRecentPathsStore.getState();
    s.push("/data/a");
    s.push("/data/b");
    s.push("/data/c");
    expect(useRecentPathsStore.getState().paths).toEqual([
      "/data/c",
      "/data/b",
      "/data/a",
    ]);
  });

  it("dedupes by moving existing entries to front", () => {
    const s = useRecentPathsStore.getState();
    s.push("/data/a");
    s.push("/data/b");
    s.push("/data/a");
    expect(useRecentPathsStore.getState().paths).toEqual([
      "/data/a",
      "/data/b",
    ]);
  });

  it("caps at 8 entries (oldest evicted)", () => {
    const s = useRecentPathsStore.getState();
    for (let i = 1; i <= 10; i++) {
      s.push(`/data/${i}`);
    }
    const paths = useRecentPathsStore.getState().paths;
    expect(paths.length).toBe(8);
    // Oldest (1, 2) evicted; newest (10) at front.
    expect(paths[0]).toBe("/data/10");
    expect(paths[7]).toBe("/data/3");
  });

  it("remove drops a single entry", () => {
    const s = useRecentPathsStore.getState();
    s.push("/data/a");
    s.push("/data/b");
    s.push("/data/c");
    useRecentPathsStore.getState().remove("/data/b");
    expect(useRecentPathsStore.getState().paths).toEqual([
      "/data/c",
      "/data/a",
    ]);
  });

  it("clear empties the list", () => {
    const s = useRecentPathsStore.getState();
    s.push("/data/a");
    s.push("/data/b");
    useRecentPathsStore.getState().clear();
    expect(useRecentPathsStore.getState().paths).toEqual([]);
  });

  it("trims whitespace before storing", () => {
    useRecentPathsStore.getState().push("  /data/x  ");
    expect(useRecentPathsStore.getState().paths).toEqual(["/data/x"]);
  });
});
