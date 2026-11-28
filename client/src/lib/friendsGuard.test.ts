import { afterEach, describe, expect, it } from "vitest";

import { friendsConfirmed, installFriendsGuard, readFriendsPlaying, useFriendsGuard } from "./friendsGuard";
import { headline13 } from "../screens/Home/WhatsNewCard";
import { friendsTitle } from "../layout/FriendsGuard";

const ENGINE = "http://127.0.0.1:19113";
const res = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

function fakeWindow(answers: Array<(headers: Record<string, string>) => Response>) {
  const calls: Array<Record<string, string>> = [];
  const w = {
    fetch: (async (_input: RequestInfo | URL, init?: RequestInit) => {
      const h = (init?.headers ?? {}) as Record<string, string>;
      calls.push(h);
      return answers[Math.min(calls.length - 1, answers.length - 1)](h);
    }) as typeof fetch,
  };
  installFriendsGuard((u) => u.startsWith(ENGINE), w);
  return { w, calls };
}

describe("the friends-playing guard (engine together_guard.rs)", () => {
  afterEach(() => useFriendsGuard.setState({ ask: null }));

  it("reads only the guard's 409", () => {
    expect(readFriendsPlaying(409, { error: "friends_playing", friends: ["Mia", 3], console: "192.168.1.20" })).toEqual({ friends: ["Mia"], console: "192.168.1.20" });
    expect(readFriendsPlaying(409, { error: "busy" })).toBeNull();
    expect(readFriendsPlaying(200, { error: "friends_playing" })).toBeNull();
  });

  it("asks once, then sends the same request with X-Confirm-Friends", async () => {
    const { w, calls } = fakeWindow([
      () => res(409, { error: "friends_playing", friends: ["Mia", "Lee"], console: "192.168.1.20" }),
      (h) => res(h["X-Confirm-Friends"] === "1" ? 200 : 409, {}),
    ]);
    const p = w.fetch(`${ENGINE}/api/ps5/power/control`, { method: "POST", body: "{}", headers: { "Content-Type": "application/json" } });
    await new Promise((r) => setTimeout(r, 0));
    const ask = useFriendsGuard.getState().ask;
    expect(ask?.friends).toEqual(["Mia", "Lee"]);
    ask?.resolve(true);
    expect((await p).status).toBe(200);
    expect(calls[1]).toEqual({ "Content-Type": "application/json", "X-Confirm-Friends": "1" });
  });

  it("Keep playing hands the 409 back; other hosts and other 409s pass through", async () => {
    const { w } = fakeWindow([() => res(409, { error: "friends_playing", friends: [] })]);
    const p = w.fetch(`${ENGINE}/api/ps5/rp/stop`, { method: "POST", body: "{}" });
    await new Promise((r) => setTimeout(r, 0));
    useFriendsGuard.getState().ask?.resolve(false);
    expect((await p).status).toBe(409);
    const other = fakeWindow([() => res(409, { error: "friends_playing" })]);
    expect((await other.w.fetch("https://example.com/x")).status).toBe(409);
    expect(useFriendsGuard.getState().ask).toBeNull();
  });

  it("the app's own yes counts for a moment", async () => {
    const { w, calls } = fakeWindow([(h) => res(h["X-Confirm-Friends"] === "1" ? 200 : 409, { error: "friends_playing" })]);
    friendsConfirmed(1000);
    expect((await w.fetch(`${ENGINE}/api/ps5/rp/stop`, { method: "POST", body: "{}" })).status).toBe(200);
    expect(calls).toHaveLength(1);
  });

  it("words and the 13.0.0 headline", () => {
    const tr = ((_k: string, v: Record<string, string> | undefined, f: string) => (v ? f : f)) as never;
    expect(friendsTitle(tr, ["Mia", "Lee"], "en")).toContain("Mia and Lee");
    expect(headline13("13.0.0")).toBe(true);
    expect(headline13("12.6.45")).toBe(false);
  });
});
