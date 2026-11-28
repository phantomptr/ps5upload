import { describe, expect, it } from "vitest";

import { canSend, outcomeText, readFriends, sendOutcome, type FriendPs5 } from "./friendsPs5";

const tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) =>
  vars ? `${key}:${JSON.stringify(vars)}|${fallback}` : `${key}|${fallback}`;

const ALEX: FriendPs5 = { id: "a1", name: "Alex", console: "192.168.1.20", madeUnix: 100, lastUnix: 200, canInvite: true };

describe("Friends' PS5s", () => {
  it("reads the engine's list, skipping what isn't a friend", () => {
    expect(
      readFriends({
        friends: [
          { id: "a1", name: " Alex ", console: "192.168.1.20", made_unix: 100, last_unix: 200, can_invite: true },
          { id: "b2", console: "192.168.1.20", made_unix: "x", can_invite: 1 },
          { name: "no id" },
          null,
        ],
      }),
    ).toEqual([ALEX, { id: "b2", name: "", console: "192.168.1.20", madeUnix: 0, lastUnix: 0, canInvite: false }]);
    expect(readFriends({})).toEqual([]);
    expect(readFriends(null)).toEqual([]);
    expect(readFriends({ friends: "x" })).toEqual([]);
  });

  it("reads how a send went", () => {
    expect(sendOutcome(200, { ok: true })).toBe("sent");
    expect(sendOutcome(200, {})).toBe("hub");
    for (const e of ["not_open", "not_playing", "no_inbox", "refused"] as const) expect(sendOutcome(409, { error: e })).toBe(e);
    expect(sendOutcome(409, { error: "refused", why: "busy" })).toBe("refused");
    expect(sendOutcome(409, { error: "else" })).toBe("hub");
    expect(sendOutcome(404, { error: "unknown" })).toBe("unknown");
    expect(sendOutcome(502, { error: "hub" })).toBe("hub");
    expect(sendOutcome(500, null)).toBe("hub");
  });

  it("offers Send invite only while Play Together is open, to an app with a waiting room", () => {
    expect(canSend(ALEX, true, new Set())).toBe(true);
    expect(canSend(ALEX, false, new Set())).toBe(false);
    expect(canSend({ ...ALEX, canInvite: false }, true, new Set())).toBe(false);
    expect(canSend(ALEX, true, new Set(["a1"]))).toBe(false);
  });

  it("says what happened, with the friend's name", () => {
    expect(outcomeText(tr, "sent", "Alex")).toBe('pt_friends_sent:{"name":"Alex"}|Sent. Alex sees it on their TV.');
    expect(outcomeText(tr, "not_open", "Alex")).toContain("PS5Upload Play isn't open on Alex's PS5 right now.");
    expect(outcomeText(tr, "not_playing", "Alex")).toBe("pt_friends_not_playing|Start playing first, then send the invite.");
    expect(outcomeText(tr, "refused", "Alex")).toContain("Alex's PS5 didn't take the invite.");
    expect(outcomeText(tr, "hub", "Alex")).toContain("pt_friends_hub|");
    expect(outcomeText(tr, "no_inbox", "Alex")).toBeNull();
    expect(outcomeText(tr, "unknown", "Alex")).toBeNull();
  });
});
