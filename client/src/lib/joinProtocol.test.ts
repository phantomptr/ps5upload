import { describe, expect, it } from "vitest";

import {
  backoffMs,
  delayTone,
  encodePadFrame,
  formatCode,
  joinSocketUrl,
  linkFromSocket,
  normalizeCode,
  parseJoinLink,
  readServerMessage,
  stickByte,
  touchToDualSense,
} from "./joinProtocol";
import { IDLE_PAD, PadButton } from "./remotePlayStream";

const bytes = (u: Uint8Array) => Array.from(u);

describe("pad frame", () => {
  it("is exactly header, seq, buttons, six axes and no touch for an idle pad", () => {
    expect(bytes(encodePadFrame(1, IDLE_PAD))).toEqual([
      0x01, 1, 0, 0, 0, 0, 0, 0, 0, 128, 128, 128, 128, 0, 0, 0,
    ]);
  });

  it("writes seq and buttons little-endian, sticks as bytes, triggers as they are", () => {
    const f = encodePadFrame(0x01020304, {
      buttons: PadButton.CROSS | PadButton.PS,
      lx: -32768,
      ly: 32767,
      rx: 0,
      ry: -256,
      l2: 255,
      r2: 7,
    });
    expect(bytes(f)).toEqual([
      0x01,
      0x04, 0x03, 0x02, 0x01,
      0x01, 0x80, 0x00, 0x00,
      0, 255, 128, 127,
      255, 7,
      0,
    ]);
  });

  it("adds one touch in the DualSense's 1920×1080 space", () => {
    const f = encodePadFrame(2, { ...IDLE_PAD, touch: [1919, 941] });
    expect(bytes(f.subarray(15))).toEqual([1, 0, 0x7f, 0x07, 0x37, 0x04]);
    expect(f.length).toBe(21);
  });

  it("appends motion as 0x02 and six float32s (rad/s, g)", () => {
    const f = encodePadFrame(3, { ...IDLE_PAD, motion: { gyro: [1000, -500, 0], accel: [0, 0, 1000], t: 1 } });
    expect(f.length).toBe(16 + 1 + 24);
    expect(f[16]).toBe(0x02);
    const dv = new DataView(f.buffer, 17, 24);
    const floats = Array.from({ length: 6 }, (_, i) => dv.getFloat32(i * 4, true));
    expect(floats).toEqual([1, -0.5, 0, 0, 0, 1]);
  });

  it("wraps seq to 32 bits", () => {
    expect(bytes(encodePadFrame(2 ** 32 + 5, IDLE_PAD).subarray(1, 5))).toEqual([5, 0, 0, 0]);
  });

  it("maps sticks and touch at their edges", () => {
    expect(stickByte(0)).toBe(128);
    expect(stickByte(-40000)).toBe(0);
    expect(stickByte(40000)).toBe(255);
    expect(touchToDualSense([0, 0])).toEqual([0, 0]);
    expect(touchToDualSense([5000, 2000])).toEqual([1919, 1079]);
  });
});

describe("engine messages", () => {
  it("reads the stages", () => {
    expect(readServerMessage('{"stage":"asking"}')).toEqual({ kind: "asking" });
    expect(readServerMessage('{"stage":"joined","player":2,"token":"abc","hevc":true}')).toEqual({
      kind: "joined",
      player: 2,
      token: "abc",
      hevc: true,
    });
    for (const why of ["key", "full", "denied", "closed", "old_helper", "not_playing"]) {
      expect(readServerMessage(JSON.stringify({ stage: "refused", why }))).toEqual({ kind: "refused", why });
    }
    expect(readServerMessage('{"stage":"refused","why":"weird"}')).toEqual({ kind: "refused", why: "refused" });
  });

  it("keeps a lost place and an unanswered ask apart from an expired invite and a no", () => {
    expect(readServerMessage('{"stage":"refused","why":"expired"}')).toEqual({ kind: "refused", why: "expired" });
    expect(readServerMessage('{"stage":"refused","why":"no_answer"}')).toEqual({ kind: "refused", why: "no_answer" });
  });

  it("says why a guest is out: another tab, or the console lost the controller", () => {
    expect(readServerMessage('{"bye":"replaced"}')).toEqual({ kind: "bye", why: "replaced" });
    expect(readServerMessage('{"bye":"lost"}')).toEqual({ kind: "bye", why: "pad_lost" });
    expect(readServerMessage('{"bye":"toString"}')).toEqual({ kind: "bye", why: "closed" });
  });

  it("reads status, feedback, bye and pong", () => {
    expect(
      readServerMessage('{"status":{"player":3,"state":"picking_user","rtt_ms":42,"host":"Ann","block_system":true}}'),
    ).toEqual({
      kind: "status",
      status: { player: 3, state: "picking_user", rttMs: 42, host: "Ann", blockSystem: true },
    });
    expect(readServerMessage('{"rumble":[300,-1],"light":[1,2,3]}')).toEqual({
      kind: "feedback",
      rumble: [255, 0],
      light: [1, 2, 3],
    });
    expect(readServerMessage('{"bye":"kicked"}')).toEqual({ kind: "bye", why: "kicked" });
    expect(readServerMessage('{"bye":"host_stopped"}')).toEqual({ kind: "bye", why: "host_stopped" });
    expect(readServerMessage('{"stage":"waiting","why":"host_stopped"}')).toEqual({ kind: "waiting" });
    expect(readServerMessage('{"bye":"other"}')).toEqual({ kind: "bye", why: "closed" });
    expect(readServerMessage('{"pong":12.5}')).toEqual({ kind: "pong", t: 12.5 });
    expect(readServerMessage("nope")).toBeNull();
    expect(readServerMessage("[1]")).toBeNull();
  });
});

describe("links and timing", () => {
  it("backs off 250 ms doubling to 4 s", () => {
    expect([0, 1, 2, 3, 4, 5, 9].map(backoffMs)).toEqual([250, 500, 1000, 2000, 4000, 4000, 4000]);
  });

  it("parses a shared link, with or without the scheme and name", () => {
    expect(parseJoinLink("http://192.168.1.9:19113/join#i=00112233445566778899aabbccddeeff&n=Ann")).toEqual({
      origin: "http://192.168.1.9:19113",
      invite: "00112233445566778899aabbccddeeff",
      name: "Ann",
      code: null,
    });
    expect(parseJoinLink(" 10.0.0.2:19113/join#i=abcdefgh12 ")?.origin).toBe("http://10.0.0.2:19113");
    expect(parseJoinLink("http://192.168.1.9:19113/pad#t=00112233445566778899aabbccddeeff")).toBeNull();
    expect(parseJoinLink("http://192.168.1.9:19113/join")).toBeNull();
    expect(parseJoinLink("ftp://x/join#i=abcdefgh12")).toBeNull();
    expect(joinSocketUrl({ origin: "http://10.0.0.2:19113" })).toBe("ws://10.0.0.2:19113/api/join/ws");
    // An engine served under a path (PS5UPLOAD_BASE_URL) keeps it.
    const prefixed = parseJoinLink("http://10.0.0.2:19113/ps5/join#i=abcdefgh12")!;
    expect(joinSocketUrl(prefixed)).toBe("ws://10.0.0.2:19113/ps5/api/join/ws");
    expect(joinSocketUrl(linkFromSocket("ws://10.0.0.2:19113/ps5/api/join/ws", "Ann")!)).toBe(
      "ws://10.0.0.2:19113/ps5/api/join/ws",
    );
  });

  it("colours a delay", () => {
    expect([null, 20, 59, 60, 119, 120, 400].map(delayTone)).toEqual(["none", "good", "good", "fair", "fair", "slow", "slow"]);
  });

  it("reads a typed short code and shows it in two halves", () => {
    expect(normalizeCode(" k7m-2qx ")).toBe("K7M2QX");
    expect(normalizeCode("K7M 2QX")).toBe("K7M2QX");
    expect(normalizeCode("K7M2Q")).toBeNull();
    expect(normalizeCode("K7M2Q!")).toBeNull();
    expect(formatCode("k7m2qx")).toBe("K7M-2QX");
  });

  it("turns a nearby host's socket into a link that asks (no invite)", () => {
    expect(linkFromSocket("ws://192.168.1.9:19113/api/join/ws", "Ann", "K7M2QX")).toEqual({
      origin: "http://192.168.1.9:19113",
      invite: null,
      code: "K7M2QX",
      name: "Ann",
    });
    expect(linkFromSocket("http://x/api/join/ws", "Ann")).toBeNull();
    expect(linkFromSocket("nope", "Ann")).toBeNull();
  });
});
