import { beforeEach, describe, expect, it } from "vitest";

import {
  MAX_EVENTS,
  RP_CLIENT_SESSIONS,
  beginRpClientSession,
  resetRpClientSessions,
  rpClientSessions,
  videoConfigFacts,
  webcodecsPath,
} from "./rpClientSessions";

describe("rpClientSessions", () => {
  beforeEach(() => resetRpClientSessions());

  it("keeps the last sessions, oldest first", () => {
    for (let i = 0; i < RP_CLIENT_SESSIONS + 3; i++) beginRpClientSession(`c${i}`, i);
    const all = rpClientSessions();
    expect(all).toHaveLength(RP_CLIENT_SESSIONS);
    expect(all[0].codec_asked).toBe("c3");
    expect(all[all.length - 1].codec_asked).toBe(`c${RP_CLIENT_SESSIONS + 2}`);
  });

  it("stops taking decoder events once a session holds MAX_EVENTS, and says so", () => {
    const s = beginRpClientSession("h264");
    for (let i = 0; i < MAX_EVENTS; i++) expect(s.event({ event: "decode_error", error: `e${i}` })).toBe(true);
    expect(s.event({ event: "decode_error", error: "one too many" })).toBe(false);
    const [r] = rpClientSessions();
    expect(r.events).toHaveLength(MAX_EVENTS);
    // A new session starts with room again.
    expect(beginRpClientSession("h264").event({ event: "configure" })).toBe(true);
  });

  it("records choices, failures with their config, fallbacks and the end", () => {
    const s = beginRpClientSession("h264");
    s.set({ video_path: "webcodecs-hw", draw_path: "gl", hdr: false });
    s.event({
      event: "decode_error",
      ...videoConfigFacts(
        { codec: "avc1.640028", hardwareAcceleration: "prefer-hardware", description: new Uint8Array(40) },
        { width: 1920, height: 1080 },
      ),
      error: "x".repeat(1000),
    });
    s.fallback("hardware gave no picture: software");
    s.end("stopped");
    s.end("later reasons do not count");
    const [r] = rpClientSessions();
    expect(r.video_path).toBe("webcodecs-hw");
    expect(r.events[0]).toMatchObject({ codec: "avc1.640028", coded_width: 1920, hw: "prefer-hardware", desc_len: 40 });
    expect(r.events[0].error!.length).toBeLessThan(400);
    expect(r.fallbacks).toEqual(["hardware gave no picture: software"]);
    expect(r.end_reason).toBe("stopped");
    expect(r.ended_at).not.toBeNull();
    // Never the description's bytes.
    expect(JSON.stringify(r)).not.toContain("\"description\"");
  });

  it("bounds events per session and hands out copies", () => {
    const s = beginRpClientSession(null);
    for (let i = 0; i < 100; i++) s.event({ event: "decode_error" });
    const a = rpClientSessions();
    expect(a[0].events.length).toBe(32);
    a[0].events.length = 0;
    expect(rpClientSessions()[0].events.length).toBe(32);
  });

  it("names the WebCodecs path", () => {
    expect(webcodecsPath("prefer-hardware")).toBe("webcodecs-hw");
    expect(webcodecsPath("prefer-software")).toBe("webcodecs-sw");
    expect(webcodecsPath(undefined)).toBe("webcodecs-default");
  });
});
