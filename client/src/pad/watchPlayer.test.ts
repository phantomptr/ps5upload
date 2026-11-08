import { describe, expect, it } from "vitest";

import { BACKLOG_LIMIT, HOST_MSE_EDGE, edgeAction } from "./watchPlayer";

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

  it("the app on the system's media player catches up by speed, and jumps only after a stall (#720)", () => {
    // A burst the busy page let pile up: no seek (each one re-decodes from a keyframe), faster.
    expect(edgeAction(0.3, 10, HOST_MSE_EDGE)).toEqual({ seekTo: null, rate: 1.1 });
    expect(edgeAction(0.05, 10, HOST_MSE_EDGE)).toEqual({ seekTo: null, rate: 1 });
    expect(edgeAction(0.7, 10, HOST_MSE_EDGE)).toEqual({ seekTo: 9.96, rate: 1 });
  });

  it("a player skips a backlog sooner", () => {
    expect(BACKLOG_LIMIT.player).toBeLessThan(BACKLOG_LIMIT.watch);
  });
});
