import { afterEach, describe, expect, it, vi } from "vitest";

import {
  draftProblem,
  emptyDraft,
  formatElapsed,
  formatKbps,
  goLiveApi,
  isLive,
  LiveApiError,
  liveFeedUrl,
  playFix,
  uploadNeededKbps,
  type LiveCheck,
  type LiveRun,
  type LiveSession,
} from "./goLive";

const session: LiveSession = { host: "10.0.0.9", codec: "h265", width: 1920, height: 1080, fps: 60, bitrate_kbps: 15000 };

const check = (code: LiveCheck["code"], limit: number | string = 0, blocking = false): LiveCheck => ({
  code,
  blocking,
  have: 0,
  limit,
  message: "",
});

describe("Go live's Add form", () => {
  it("needs a key for the sites, and a server for Kick", () => {
    expect(draftProblem({ ...emptyDraft("youtube") })).toBe("key_missing");
    expect(draftProblem({ ...emptyDraft("youtube"), key: "abcd-efgh-ijkl" })).toBeNull();
    expect(draftProblem({ ...emptyDraft("twitch"), key: "live_1 2" })).toBe("key_invalid");
    expect(draftProblem({ ...emptyDraft("twitch"), key: "rtmp://x/live_1" })).toBe("key_invalid");
    expect(draftProblem({ ...emptyDraft("twitch"), key: "a|bcd" })).toBe("key_invalid");
    // Facebook keys carry a query.
    expect(draftProblem({ ...emptyDraft("facebook"), key: "FB-1-0-Ab?s_bl=1&s_psm=1" })).toBeNull();
    expect(draftProblem({ ...emptyDraft("kick"), key: "sk_abc_123" })).toBe("server_missing");
    expect(draftProblem({ ...emptyDraft("kick"), key: "sk_abc_123", server: "rtmp://x/app" })).toBe("url_invalid");
    expect(
      draftProblem({ ...emptyDraft("kick"), key: "sk_abc_123", server: "rtmps://abc.global-contribute.live-video.net/app" }),
    ).toBeNull();
  });

  it("takes any rtmp, rtmps or srt server for Custom, the key optional", () => {
    expect(draftProblem({ ...emptyDraft("custom"), server: "" })).toBe("url_invalid");
    expect(draftProblem({ ...emptyDraft("custom"), server: "http://x/live" })).toBe("url_invalid");
    expect(draftProblem({ ...emptyDraft("custom"), server: "rtmp://192.168.1.5/live" })).toBeNull();
    expect(draftProblem({ ...emptyDraft("custom"), server: "srt://host:9000?latency=200" })).toBeNull();
    expect(draftProblem({ ...emptyDraft("custom"), server: "rtmp://h/live", key: "a b" })).toBe("key_invalid");
  });

  it("starts Twitch in test mode", () => {
    expect(emptyDraft("twitch").bandwidth_test).toBe(true);
    expect(emptyDraft("youtube").bandwidth_test).toBe(false);
  });
});

describe("Go live's numbers", () => {
  it("formats the LIVE chip's time and bitrate", () => {
    expect(formatElapsed(0)).toBe("0:00");
    expect(formatElapsed(75)).toBe("1:15");
    expect(formatElapsed(3600 + 62)).toBe("1:01:02");
    expect(formatKbps(6123)).toBe("6.1 Mbps");
    expect(formatKbps(850.4)).toBe("850 kbps");
    expect(formatKbps(NaN)).toBe("0 kbps");
  });

  it("estimates the upload needed per site", () => {
    expect(uploadNeededKbps(null, 1)).toBe(0);
    expect(uploadNeededKbps({ ...session, bitrate_kbps: 6000 }, 1)).toBe(Math.round(6160 * 1.2));
    expect(uploadNeededKbps({ ...session, bitrate_kbps: 6000 }, 2)).toBe(Math.round(6160 * 1.2 * 2));
  });

  it("is live until ended or failed", () => {
    const run = (phase: LiveRun["phase"]) => ({ phase }) as LiveRun;
    expect(isLive(null)).toBe(false);
    expect(["starting", "live", "reconnecting", "waiting_for_play"].every((p) => isLive(run(p as LiveRun["phase"])))).toBe(true);
    expect(isLive(run("ended")) || isLive(run("failed"))).toBe(false);
  });

  it("works out the Play restart the chosen sites need", () => {
    expect(playFix(null, [{ checks: [check("codec", 0, true)] }])).toBeNull();
    expect(playFix(session, [{ checks: [] }, {}])).toBeNull();
    // Twitch (H.264, 6 Mbps) and Kick (8 Mbps): H.264 at the lower of the two.
    expect(
      playFix(session, [
        { checks: [check("codec", 0, true), check("bitrate", 6000)] },
        { checks: [check("codec", 0, true), check("bitrate", 8000)] },
      ]),
    ).toEqual({ codec: "h264", bitrateKbps: 6000 });
    expect(playFix(session, [{ checks: [check("hdr", 0, true)] }])).toEqual({ codec: "h264", bitrateKbps: null });
    expect(playFix(session, [{ checks: [check("bitrate", 9000)] }])).toEqual({ codec: null, bitrateKbps: 9000 });
  });

  it("gives OBS the engine's live.ts", () => {
    expect(liveFeedUrl("http://127.0.0.1:19113/")).toBe("http://127.0.0.1:19113/api/ps5/rp/live.ts");
  });
});

describe("Go live's API calls", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("sends saved destinations by id, never keys", async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify({ ok: true, live: {} }), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    await goLiveApi.start(["twitch-abc123", "youtube-def456"], 160);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toMatch(/\/api\/stream\/start$/);
    expect(JSON.parse(init.body as string)).toEqual({
      destinations: [{ key_id: "twitch-abc123" }, { key_id: "youtube-def456" }],
      audio_kbps: 160,
    });
  });

  it("adds a Kick destination with its server, a custom one with its URL", async () => {
    const fetchMock = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    await goLiveApi.add({ preset: "kick", key: " sk_1 ", server: "rtmps://k/app", label: "", bandwidth_test: false });
    await goLiveApi.add({ preset: "custom", key: "", server: "srt://h:9000", label: "Mine", bandwidth_test: true });
    const bodies = (fetchMock.mock.calls as unknown as Array<[string, RequestInit]>).map((c) => JSON.parse(c[1].body as string));
    expect(bodies[0]).toEqual({ preset: "kick", key: "sk_1", server: "rtmps://k/app" });
    expect(bodies[1]).toEqual({ preset: "custom", key: "", url: "srt://h:9000", label: "Mine" });
  });

  it("turns a refusal into its code and sentence", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => new Response(JSON.stringify({ error: "ffmpeg_missing", message: "Download it" }), { status: 412 })),
    );
    const e = await goLiveApi.start(["x"]).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(LiveApiError);
    expect((e as LiveApiError).code).toBe("ffmpeg_missing");
    vi.stubGlobal("fetch", vi.fn(async () => new Response("not json", { status: 404 })));
    const e2 = await goLiveApi.status().catch((x: unknown) => x);
    expect((e2 as LiveApiError).code).toBe("http_404");
  });
});
