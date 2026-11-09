import { describe, expect, it } from "vitest";

import { RP_BITRATES } from "../../state/remotePlay";
import { linkGrade, lowerForAll, readJoinState, readLink, SUGGEST_AFTER_S, type GuestLink, type JoinGuest } from "./playTogetherState";

const link = (o: Partial<GuestLink>): GuestLink => ({
  measuring: false,
  kbps: 30_000,
  streamKbps: 15_000,
  fitKbps: 25_000,
  underS: 0,
  loss: 0,
  rttMs: 20,
  ...o,
});

describe("a guest's connection check (the host's panel)", () => {
  it("reads the engine's track numbers", () => {
    expect(
      readLink({ track: { measuring: false, bwe_kbps: 9000, stream_kbps: 15000, fit_kbps: 7500, under_s: 12, loss: 0.01, rtcp_rtt_ms: 40 } }),
    ).toEqual({ measuring: false, kbps: 9000, streamKbps: 15000, fitKbps: 7500, underS: 12, loss: 0.01, rttMs: 40 });
    // Off the track (the data channel, the plain socket, an older engine): no check.
    expect(readLink({ track: null })).toBeNull();
    expect(readLink(undefined)).toBeNull();
  });

  it("grades it with about how much it carries", () => {
    expect(linkGrade(null)).toBeNull();
    expect(linkGrade(link({ measuring: true }))).toEqual({ grade: "measuring", mbps: null });
    expect(linkGrade(link({ kbps: null }))).toEqual({ grade: "measuring", mbps: null });
    expect(linkGrade(link({}))).toEqual({ grade: "good", mbps: 30 });
    // Under 20% to spare, some loss or a long round trip: OK.
    expect(linkGrade(link({ kbps: 16_000 }))?.grade).toBe("ok");
    expect(linkGrade(link({ loss: 0.03 }))?.grade).toBe("ok");
    expect(linkGrade(link({ rttMs: 90 }))?.grade).toBe("ok");
    // Under the stream's rate, heavy loss or a very long round trip: slow.
    expect(linkGrade(link({ kbps: 6_200, underS: 3 }))).toEqual({ grade: "slow", mbps: 6.2 });
    expect(linkGrade(link({ loss: 0.08 }))?.grade).toBe("slow");
    expect(linkGrade(link({ rttMs: 200 }))?.grade).toBe("slow");
  });
});

describe("a lower rate for everyone", () => {
  const guest = (id: number, l: GuestLink | null, status: JoinGuest["status"] = "playing") =>
    ({ ...readJoinState({ guests: [{ id, name: `G${id}`, player: id + 1, state: status }] }).guests[0], link: l }) as JoinGuest;

  it("is offered only once a guest has stayed under the rate long enough", () => {
    expect(lowerForAll([guest(1, link({ underS: SUGGEST_AFTER_S - 1, fitKbps: 5000 }))], RP_BITRATES)).toBeNull();
    expect(lowerForAll([guest(1, link({}))], RP_BITRATES)).toBeNull();
    expect(lowerForAll([guest(1, link({ underS: 30, fitKbps: 5000 }), "reconnecting")], RP_BITRATES)).toBeNull();
  });

  it("fits the slowest guest with room to spare, under the rate now", () => {
    const o = lowerForAll(
      [guest(1, link({ underS: 12, kbps: 9000, fitKbps: 7500 })), guest(2, link({ underS: 20, kbps: 6000, fitKbps: 5000 })), guest(3, link({}))],
      RP_BITRATES,
    );
    expect(o?.guest.id).toBe(2);
    expect(o?.toKbps).toBe(4500);
    expect(o?.gotKbps).toBe(6000);
    // Nothing fits: the lowest there is.
    expect(lowerForAll([guest(1, link({ underS: 12, fitKbps: 1000 }))], RP_BITRATES)?.toKbps).toBe(3000);
    // Already at the lowest: nothing to offer.
    expect(lowerForAll([guest(1, link({ underS: 12, fitKbps: 1000, streamKbps: 3000 }))], RP_BITRATES)).toBeNull();
  });
});
