import { describe, expect, it } from "vitest";

import { BACKLOG_LIMIT, edgeAction } from "./watchPlayer";

describe("watch player latency", () => {
  it("a player keeps the picture within 150 ms of the live edge", () => {
    expect(edgeAction(0.03, 10, "player")).toEqual({ seekTo: null, rate: 1 });
    expect(edgeAction(0.1, 10, "player")).toEqual({ seekTo: null, rate: 1.1 });
    expect(edgeAction(0.2, 10, "player")).toEqual({ seekTo: 9.98, rate: 1 });
  });

  it("a watcher may sit half a second back, as before", () => {
    expect(edgeAction(0.4, 10, "watch")).toEqual({ seekTo: null, rate: 1 });
    expect(edgeAction(0.6, 10, "watch")).toEqual({ seekTo: 9.95, rate: 1 });
  });

  it("a player skips a backlog sooner", () => {
    expect(BACKLOG_LIMIT.player).toBeLessThan(BACKLOG_LIMIT.watch);
  });
});
