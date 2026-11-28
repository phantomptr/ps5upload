import { beforeEach, describe, expect, it } from "vitest";

import { installWindowStub } from "../../test-utils/storage";
import { hostRoom, hostRoomBinary, hostRoomText } from "./hostRoom";

describe("the host's room on the Remote Play socket", () => {
  beforeEach(() => {
    installWindowStub();
    hostRoom.reset();
  });

  it("takes the room's messages, leaves the stream's own", () => {
    expect(hostRoomText(JSON.stringify({ stage: "streaming" }))).toBe(false);
    expect(hostRoomText(JSON.stringify({ stats: { chat: 1 } }))).toBe(false);
    expect(hostRoomText(JSON.stringify({ comms: { notice: 1, accepted: true, chat: "on", voice: "ptt", caps: ["voice"] } }))).toBe(true);
    expect(hostRoom.snap().voiceMode).toBe("ptt");
    expect(hostRoomText(JSON.stringify({ chat: { id: 1, sid: 0, from: "Sam", host: true, t: 1, quick: 3 } }))).toBe(true);
    expect(hostRoom.snap().lines).toHaveLength(1);
    expect(hostRoomText(JSON.stringify({ flagged: { by: "Mia", who: "Lee" } }))).toBe(true);
    expect(hostRoomText("not json")).toBe(false);
  });

  it("voice bundles (0x0B) are the room's; pictures and sound are not", () => {
    let got = 0;
    hostRoom.onVoice = () => got++;
    expect(hostRoomBinary(new Uint8Array([0x0b, 0]).buffer)).toBe(true);
    expect(hostRoomBinary(new Uint8Array([7, 0, 0]).buffer)).toBe(false);
    expect(got).toBe(1);
    hostRoom.onVoice = null;
  });

  it("the mic switch goes as room_mic", () => {
    const sent: Array<Record<string, unknown>> = [];
    hostRoom.sendJson = (m) => (sent.push(m), true);
    hostRoom.mic("on");
    expect(sent).toEqual([{ room_mic: "on" }]);
  });
});
