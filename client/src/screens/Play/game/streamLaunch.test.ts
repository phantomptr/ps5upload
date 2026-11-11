// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";

import { beforeEach, describe, expect, it } from "vitest";

import { useStreamLaunch } from "./streamLaunch";

// Home's Play card asks the Play screen for the stream instead of starting it: the screen's
// Start has guards (leave a friend's game first, a payload that blocks the picture).
describe("a stream asked for from another screen", () => {
  beforeEach(() => useStreamLaunch.setState({ pending: null, streamOnly: null }));

  it("is handed to the stream panel once", () => {
    useStreamLaunch.getState().requestStream("192.168.1.5");
    expect(useStreamLaunch.getState().streamOnly).toBe("192.168.1.5");
    expect(useStreamLaunch.getState().takeStream()).toBe("192.168.1.5");
    expect(useStreamLaunch.getState().streamOnly).toBeNull();
    expect(useStreamLaunch.getState().takeStream()).toBeNull();
  });

  it("is not a game launch, and leaves one alone", () => {
    useStreamLaunch.getState().request("192.168.1.5", { titleId: "PPSA01234", titleName: "Game" });
    useStreamLaunch.getState().requestStream("192.168.1.5");
    expect(useStreamLaunch.getState().pending?.titleId).toBe("PPSA01234");
    useStreamLaunch.getState().takeStream();
    expect(useStreamLaunch.getState().pending?.titleId).toBe("PPSA01234");
  });
});

describe("Home's Play card", () => {
  const card: string = readFileSync(new URL("../../Home/PlayCard.tsx", import.meta.url), "utf8");
  it("asks the Play screen for the stream and never starts Remote Play by itself", () => {
    expect(card).toContain("useStreamLaunch.getState().requestStream(profile.host)");
    expect(card).not.toMatch(/useRemotePlayStore\s*\.getState\(\)\s*\.start\(/);
  });
});
