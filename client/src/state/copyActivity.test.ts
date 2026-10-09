import { describe, expect, it } from "vitest";

import { activityFor } from "./copyActivity";
import type { QueueItem } from "./uploadQueue";

const game = "/games/Astro Bot";
const idle = { phase: "idle" } as const;

function sending(addr: string): QueueItem {
  return {
    id: addr,
    sourceKind: "folder",
    sourcePath: game,
    addr,
    status: "running",
    bytesSent: 50,
    totalBytes: 100,
    estimatedBytes: 0,
  } as unknown as QueueItem;
}

describe("activityFor", () => {
  it("is busy on the console the game is going to", () => {
    expect(activityFor(game, [sending("192.168.0.100")], idle, "192.168.0.100")).toMatchObject({
      phase: "sending",
      pct: 50,
    });
  });

  it("leaves the same game free to send to another console", () => {
    // Sending a game to the Pro disabled its Send button for the Phat too.
    expect(activityFor(game, [sending("192.168.0.100")], idle, "192.168.0.99")).toBeNull();
  });

  it("matches a console address with a port", () => {
    expect(activityFor(game, [sending("192.168.0.100:9113")], idle, "192.168.0.100")).not.toBeNull();
  });

  it("shows an image being built for another console as no activity on this one", () => {
    const building = {
      phase: "running",
      source: game,
      host: "192.168.0.100",
      stageDone: 1,
      stageTotal: 4,
    } as unknown as Parameters<typeof activityFor>[2];
    expect(activityFor(game, [], building, "192.168.0.99")).toBeNull();
    expect(activityFor(game, [], building, "192.168.0.100")).toMatchObject({ phase: "building" });
  });
});
