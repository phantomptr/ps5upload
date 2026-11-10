import { describe, expect, it } from "vitest";

import { videoRecordBytes, type StreamStats } from "../../lib/remotePlayStream";
import { latencyParts } from "./NetworkExtras";
import { statsLineValues } from "./StreamPanel";

describe("latencyParts", () => {
  it("rounds the round trip to the engine and the decode time", () => {
    expect(latencyParts({ rttMs: 8.6, decodeMs: 4.2 })).toEqual({ engine: 9, decode: 4 });
  });

  it("has no engine part until the engine answered a ping", () => {
    expect(latencyParts({ rttMs: null, decodeMs: 3.7 })).toEqual({ engine: null, decode: 4 });
  });

  it("says ? for the decode time where the system's media player decodes, not 0 (#720)", () => {
    expect(latencyParts({ rttMs: 2, decodeMs: null })).toEqual({ engine: 2, decode: "?" });
  });
});

describe("the stats line (#720)", () => {
  const stats: StreamStats = {
    fps: 30,
    mbps: 4.46,
    decodeMs: null,
    codec: "avc1",
    lossPct: 0,
    rebuilt: 0,
    lost: 0,
    audioMs: 35,
    underruns: 2,
    rttMs: 2,
  };

  it("has no made-up 0.0 ms under the media player, and a measured time otherwise", () => {
    expect(statsLineValues(stats)).toMatchObject({ fps: 30, mbps: "4.5", ms: "?", codec: "H.264" });
    expect(statsLineValues({ ...stats, decodeMs: 3.04 }).ms).toBe("3.0");
  });

  it("counts the media player's pictures in the Mbit/s", () => {
    // [kind][AU…] and [kind][8-byte time][AU…]: the picture bytes only.
    expect(videoRecordBytes(1, 1001)).toBe(1000);
    expect(videoRecordBytes(7, 1009)).toBe(1000);
    expect(videoRecordBytes(7, 4)).toBe(0);
    expect(videoRecordBytes(3, 500)).toBe(0);
  });
});
