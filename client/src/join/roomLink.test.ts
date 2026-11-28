import { beforeEach, describe, expect, it } from "vitest";

import { installWindowStub } from "../test-utils/storage";

import { readServerMessage } from "../lib/joinProtocol";
import { acceptNotice } from "../lib/room/notice";
import { commsRefusedText, mergeLines, RoomLink } from "./roomLink";

const tr = (_k: string, v: Record<string, string | number> | undefined, f: string) => (v ? f.replace(/\{(\w+)\}/g, (_m, k: string) => String(v[k])) : f);

function feed(link: RoomLink, v: unknown) {
  const m = readServerMessage(JSON.stringify(v));
  if (!m) throw new Error("not read");
  switch (m.kind) {
    case "comms":
      return link.onComms(m.comms);
    case "voice":
      return link.onVoiceState(m.voice);
    case "chat":
      return link.onChat(m.line);
    case "chat_history":
      return link.onChatHistory(m.lines);
    case "comms_refused":
      return link.onRefused(m.refused);
    case "muted":
      return link.onMuted({ voice: m.voice, chat: m.chat });
    default:
      throw new Error(m.kind);
  }
}

describe("room link (API.md Play Rooms chat and voice)", () => {
  beforeEach(() => void installWindowStub());

  it("reads the engine's messages in order: comms, voice, history, lines with folds", () => {
    const sent: Array<Record<string, unknown>> = [];
    const link = new RoomLink(() => 1000);
    link.sendJson = (m) => (sent.push(m), true);
    feed(link, { comms: { notice: 1, accepted: false, chat: "quick", voice: "ptt", caps: ["chat", "voice"] } });
    // Not accepted on this device: nothing goes.
    expect(sent).toEqual([]);
    feed(link, {
      voice: {
        mode: "ptt",
        you: 2,
        mic: "off",
        ptt: false,
        paused: null,
        stepped: false,
        kbps: 12,
        frame_ms: 40,
        speaking: [],
        people: [
          { sid: 0, name: "Sam", player: 1, host: true, muted: false, paused: false, mic: true },
          { sid: 2, name: "Jo", player: 2, host: false, muted: false, paused: false, mic: false },
        ],
      },
    });
    expect(link.snap()).toMatchObject({ you: 2, kbps: 12, frameMs: 40, voiceMode: "ptt" });
    expect(link.snap().people.map((p) => p.name)).toEqual(["Sam", "Jo"]);
    feed(link, { voice: { speaking: [0] } });
    expect(link.snap().speaking).toEqual([0]);
    expect(link.snap().people).toHaveLength(2);
    feed(link, { chat_history: [{ id: 4, sid: 0, from: "Sam", player: 1, host: true, t: 3, quick: 1 }] });
    feed(link, { chat: { id: 5, sid: 2, from: "Jo", player: 2, host: false, t: 4, text: "hi" } });
    feed(link, { chat: { id: 5, sid: 2, from: "Jo", player: 2, host: false, t: 5, text: "hi", x: 2 } });
    expect(link.snap().lines.map((l) => [l.n, l.from, l.mine, l.times])).toEqual([
      [4, "Sam", false, 1],
      [5, "Jo", true, 2],
    ]);
    feed(link, { muted: { voice: true, chat: false } });
    expect(link.snap().muted).toEqual({ voice: true, chat: false });
  });

  it("tells the engine the notice once accepted here, and asks for it on a refusal", () => {
    const sent: Array<Record<string, unknown>> = [];
    const link = new RoomLink();
    link.sendJson = (m) => (sent.push(m), true);
    feed(link, { comms_refused: { what: "chat", why: "notice", notice: 1 } });
    expect(link.snap().needNotice).toBe(true);
    acceptNotice();
    feed(link, { comms: { notice: 1, accepted: false, chat: "on", voice: "open", caps: [] } });
    expect(sent).toEqual([{ notice: 1 }]);
    expect(link.snap().needNotice).toBe(false);
  });

  it("sends chat, mic (room_mic on the host page) and flags once", () => {
    const sent: Array<Record<string, unknown>> = [];
    const link = new RoomLink();
    link.sendJson = (m) => (sent.push(m), true);
    link.chat({ quick: 3 });
    link.mic("ptt_down");
    link.micKey = "room_mic";
    link.mic("off");
    link.flag(4);
    link.flag(4);
    expect(sent).toEqual([{ chat: { quick: 3 } }, { mic: "ptt_down" }, { room_mic: "off" }, { flag: 4 }]);
  });

  it("every refusal in words", () => {
    for (const why of ["notice", "off", "quick_only", "muted", "slow_down", "empty", "too_long", "bad_quick", "ptt_only", "mic_off", "poor", "not_open", "x"]) {
      expect(commsRefusedText(tr, { why, retryMs: 2500 }).length, why).toBeGreaterThan(3);
    }
    expect(commsRefusedText(tr, { why: "slow_down", retryMs: 2500 })).toBe("Slow down. Try again in 3 s.");
    expect(mergeLines([], [], null, 0)).toEqual([]);
  });
});
