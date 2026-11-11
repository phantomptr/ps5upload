import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../state/remotePlay", async () => {
  const { create } = await import("zustand");
  return { useRemotePlayStore: create(() => ({ stream: null, target: null, prefs: {} })) };
});

import type { LiveCheck, LiveRun, StreamStatus } from "../../lib/goLive";
import { useGoLiveStore } from "../../state/goLive";
import { GoLiveChip, GoLivePanel, goLiveAvailable, obsFeedAvailable, ObsFeedView, problemText } from "./GoLivePanel";
import { removeAfterConfirm, removeConfirmOptions } from "./goLiveText";

// No DOM here (vitest runs in node): static markup, asserting on what is shown.
const html = (node: React.ReactNode) => renderToStaticMarkup(<>{node}</>);

const preset = (id: string, name: string, extra: Partial<StreamStatus["presets"][number]> = {}) =>
  ({
    id,
    name,
    server: null,
    needs_server: false,
    codecs: ["h264"],
    max_kbps: 6000,
    max_height: 1080,
    max_fps: 60,
    key_page: "",
    bandwidth_test: false,
    checks: [],
    ...extra,
  }) as StreamStatus["presets"][number];

function status(over: Partial<StreamStatus> = {}): StreamStatus {
  return {
    ffmpeg: {
      installed: true,
      version: "9.0.2",
      supported: true,
      download_bytes: 28_395_699,
      source: null,
      install: { running: false, step: null, done_bytes: 0, total_bytes: 0, error: null, message: null },
    },
    session: { host: "10.0.0.9", codec: "h264", width: 1920, height: 1080, fps: 60, bitrate_kbps: 15000 },
    presets: [preset("youtube", "YouTube"), preset("twitch", "Twitch")],
    destinations: [],
    store_error: null,
    live: null,
    ...over,
  };
}

/** A saved site, ready to go live to. */
const youtube: StreamStatus["destinations"][number] = {
  id: "yt-1",
  preset: "youtube",
  label: "YouTube",
  server: "",
  bandwidth_test: false,
  enabled: true,
  key_hint: "…abcd",
  name: "YouTube",
  checks: [],
};

const run = (over: Partial<LiveRun> = {}): LiveRun => ({
  phase: "live",
  error: null,
  started_s: 70,
  live_s: 65,
  reconnects: 0,
  retry_in_s: null,
  kbps: 6120,
  fps: 59.9,
  speed: 1,
  frames: 3900,
  dropped_frames: 0,
  upload_skips: 0,
  slow_upload: false,
  audio_kbps: 160,
  destinations: [
    { id: "t1", label: "Twitch", preset: "twitch", name: "Twitch", state: "live", error: null, bandwidth_test: true },
    {
      id: "y1",
      label: "My channel",
      preset: "youtube",
      name: "YouTube",
      state: "retrying",
      error: { code: "dropped", message: "x" },
      bandwidth_test: false,
    },
  ],
  session: null,
  ...over,
});

// Static rendering reads a zustand store's server snapshot, its initial state: set both.
const set = (s: Partial<ReturnType<typeof useGoLiveStore.getState>>) => {
  useGoLiveStore.setState(s);
  Object.assign(useGoLiveStore.getInitialState(), s);
};

describe("The Stream panel", () => {
  afterEach(() => set({ status: null, loadError: null, actionError: null, busy: false }));

  it("is offered everywhere the engine runs (#731, #763)", () => {
    // Desktop app: its engine is on the computer.
    expect(goLiveAvailable({ tauri: true, mobile: false })).toBe(true);
    // The phone apps: their own engine streams without ffmpeg (the status says if it can't).
    expect(goLiveAvailable({ tauri: true, mobile: true })).toBe(true);
    // The web UI in a phone's Safari: the engine (and ffmpeg) is on a computer.
    expect(goLiveAvailable({ tauri: false, mobile: true })).toBe(true);
    expect(goLiveAvailable({ tauri: false, mobile: false })).toBe(true);
    // OBS reads the feed on the engine's computer only.
    expect(obsFeedAvailable({ mobile: false, engineUrl: "http://127.0.0.1:19113" })).toBe(true);
    expect(obsFeedAvailable({ mobile: false, engineUrl: "http://192.168.1.5:19113" })).toBe(false);
    expect(obsFeedAvailable({ mobile: true, engineUrl: "http://127.0.0.1:19113" })).toBe(false);
  });

  it("asks to download ffmpeg first, with its size", () => {
    const s = status();
    s.ffmpeg.installed = false;
    set({ status: s });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("PS5Upload needs a free helper (ffmpeg, about 28 MB) to convert the sound for streaming sites. Download it?");
    expect(out).toContain('data-testid="golive-ffmpeg-download"');
  });

  it("needs no ffmpeg when the engine streams by itself (the phone apps, #763)", () => {
    const s = status({ pipeline: "native", destinations: [youtube] });
    s.ffmpeg.installed = false;
    s.ffmpeg.supported = false;
    set({ status: s });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).not.toContain('data-testid="golive-ffmpeg"');
    expect(out).not.toContain('data-testid="golive-native-unavailable"');
    expect(out).toMatch(/<button[^>]*data-testid="golive-start"/);
    expect(out).not.toMatch(/<button[^>]*disabled=""[^>]*data-testid="golive-start"/);
  });

  it("says when a phone can't convert the sound, and offers no start", () => {
    const s = status({ pipeline: "none", destinations: [youtube] });
    s.ffmpeg.installed = false;
    set({ status: s });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain('data-testid="golive-native-unavailable"');
    expect(out).toContain("going live isn&#x27;t available here");
    expect(out).not.toContain('data-testid="golive-ffmpeg"');
    expect(out).toMatch(/<button[^>]*disabled=""[^>]*data-testid="golive-start"/);
  });

  it("says what to do when the ffmpeg download can't run here (a Docker image)", () => {
    const s = status();
    s.ffmpeg.installed = false;
    s.ffmpeg.supported = false;
    s.ffmpeg.needs_own = true;
    set({ status: s });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("PS5UPLOAD_FFMPEG");
    expect(out).not.toContain('data-testid="golive-ffmpeg-download"');
  });

  it("shows a store that can't be read, with the file to look at", () => {
    set({ status: status({ store_error: "reading /data/stream-destinations.json: expected value" }) });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain('data-testid="golive-store-error"');
    expect(out).toContain("/data/stream-destinations.json");
  });

  it("says a saved key can't be read", () => {
    set({
      status: status({
        destinations: [
          { id: "t1", preset: "twitch", label: "Twitch", server: "", bandwidth_test: false, enabled: true, key_hint: "", key_lost: true, name: "Twitch" },
        ],
      }),
    });
    expect(html(<GoLivePanel onClose={() => {}} />)).toContain('data-testid="golive-key-lost"');
  });

  it("shows Stopping, with Stop off, until ffmpeg has gone", () => {
    set({ status: status({ live: run({ phase: "stopping" }) }) });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("Stopping…");
    expect(out).toMatch(/<button[^>]*data-testid="golive-stop"[^>]*disabled|<button[^>]*disabled[^>]*data-testid="golive-stop"/);
    expect(out).not.toContain("LIVE");
  });

  it("offers each site to add when there is none", () => {
    set({ status: status() });
    const out = html(<GoLivePanel onClose={() => {}} />);
    for (const s of ["youtube", "twitch", "kick", "facebook", "custom"]) {
      expect(out).toContain(`data-testid="golive-add-${s}-button"`);
    }
    expect(out).not.toContain('data-testid="golive-start"');
  });

  it("lists destinations with the key's end only, warns, and offers the Play restart", () => {
    set({
      status: status({
        destinations: [
          {
            id: "twitch-1",
            preset: "twitch",
            label: "Twitch",
            server: "",
            bandwidth_test: true,
            enabled: true,
            key_hint: "…WXYZ",
            name: "Twitch",
            checks: [{ code: "bitrate", blocking: false, have: 15000, limit: 6000, message: "" }],
          },
        ],
      }),
    });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("…WXYZ");
    expect(out).toContain("Your Play bitrate 15 Mbps is above Twitch&#x27;s 6 Mbps. Lower it in Play settings (Picture) before going live.");
    expect(out).toContain("Restart Remote Play at 6 Mbps?");
    expect(out).toContain("Test without going public");
    expect(out).toContain("Needs about 18.2 Mbps of upload.");
    // Not blocked by a warning: Go live is on.
    expect(out).toMatch(/<button[^>]*data-testid="golive-start"(?![^>]*disabled)/);
  });

  it("blocks Go live when a chosen site refuses the codec", () => {
    set({
      status: status({
        session: { host: "h", codec: "h265", width: 1920, height: 1080, fps: 60, bitrate_kbps: 6000 },
        destinations: [
          {
            id: "kick-1",
            preset: "kick",
            label: "Kick",
            server: "rtmps://k/app",
            bandwidth_test: false,
            enabled: true,
            key_hint: "…abcd",
            name: "Kick",
            checks: [{ code: "codec", blocking: true, have: "h265", limit: ["h264"], message: "" }],
          },
        ],
      }),
    });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("Kick takes H.264 only, and Remote Play is sending HEVC.");
    expect(out).toContain("Restart Remote Play in H.264?");
    expect(out).toMatch(/<button[^>]*disabled[^>]*data-testid="golive-start"|<button[^>]*data-testid="golive-start"[^>]*disabled/);
  });

  it("offers the restart for this session only, and again for a later session in HEVC", () => {
    const withKick = (session: StreamStatus["session"], checks: LiveCheck[]) =>
      status({
        session,
        destinations: [
          {
            id: "kick-1",
            preset: "kick",
            label: "Kick",
            server: "rtmps://k/app",
            bandwidth_test: false,
            enabled: true,
            key_hint: "…abcd",
            name: "Kick",
            checks,
          },
        ],
      });
    const hevc: NonNullable<StreamStatus["session"]> = { host: "h", codec: "h265", width: 1920, height: 1080, fps: 60, bitrate_kbps: 6000 };
    const refused: LiveCheck[] = [{ code: "codec", blocking: true, have: "h265", limit: ["h264"], message: "" }];
    const blockedStart = /<button[^>]* disabled=""[^>]*data-testid="golive-start"|<button[^>]*data-testid="golive-start"[^>]* disabled=""/;

    set({ status: withKick(hevc, refused) });
    const first = html(<GoLivePanel onClose={() => {}} />);
    expect(first).toContain("This lasts for this session only");
    expect(first).not.toContain("until you change");
    expect(first).toContain("golive-fix-restart");
    expect(first).toMatch(blockedStart);

    // Restarted in H.264: the engine's check passes, so nothing asks and Go live is on.
    set({ status: withKick({ ...hevc, codec: "h264" }, []) });
    const restarted = html(<GoLivePanel onClose={() => {}} />);
    expect(restarted).not.toContain("golive-fix-restart");
    expect(restarted).not.toMatch(blockedStart);

    // The next session is back on the Picture settings: it asks again instead of failing at the site.
    set({ status: withKick(hevc, refused) });
    const later = html(<GoLivePanel onClose={() => {}} />);
    expect(later).toContain("golive-fix-restart");
    expect(later).toMatch(blockedStart);
  });

  it("shows the live run per site, and Stop", () => {
    set({ status: status({ live: run() }) });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("LIVE");
    expect(out).toContain("1:05");
    expect(out).toContain("6.1 Mbps · 60 fps");
    expect(out).toContain("test, not public");
    expect(out).toContain("The connection to YouTube dropped.");
    expect(out).toContain('data-testid="golive-stop"');
  });

  it("says when it reconnects, and the chip shows on the picture", () => {
    set({ status: status({ live: run({ phase: "reconnecting", retry_in_s: 4, error: { code: "unreachable", message: "" } }) }) });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("Trying again in 4 s.");
    expect(out).toContain("Can&#x27;t reach the site&#x27;s server.");
    expect(html(<GoLiveChip />)).toContain("Reconnecting…");
    set({ status: status({ live: run({ phase: "ended" }) }) });
    expect(html(<GoLiveChip />)).toBe("");
  });

  it("explains a refused key after the stream stopped", () => {
    set({
      status: status({
        live: run({
          phase: "failed",
          error: { code: "rejected", message: "" },
          destinations: [{ id: "t", label: "Twitch", preset: "twitch", name: "Twitch", state: "failed", error: null, bandwidth_test: false }],
        }),
      }),
    });
    const out = html(<GoLivePanel onClose={() => {}} />);
    expect(out).toContain("Twitch refused the stream. That is almost always the stream key");
  });

  it("says when the engine is too old for Go live", () => {
    set({ status: null, loadError: "http_404" });
    expect(html(<GoLivePanel onClose={() => {}} />)).toContain("This engine can&#x27;t go live");
  });

  it("gives OBS the four steps and whether it reads", () => {
    const out = html(
      <ObsFeedView
        url="http://127.0.0.1:19113/api/ps5/rp/live.ts"
        status={{ running: true, consumers: 1, height: 1080, fps: 60, codec: "h264" }}
        copied={false}
        onCopy={() => {}}
      />,
    );
    expect(out).toContain("http://127.0.0.1:19113/api/ps5/rp/live.ts");
    expect(out).toContain("Input format: mpegts");
    expect(out).toContain("OBS connected (1)");
    expect(out).toContain("1080p 60 fps");
  });
});

describe("engine error codes", () => {
  const tr = ((_k: string, _v: unknown, fallback?: string) => fallback ?? _k) as unknown as Parameters<typeof problemText>[0];

  it("never shows the engine's own English for a code it knows", () => {
    for (const code of [
      "unreachable",
      "tls",
      "rejected",
      "codec_not_allowed",
      "dropped",
      "stopped",
      "all_failed",
      "ffmpeg_failed",
      "destination_missing",
      "not_found",
      "preset_unknown",
      "key_lost",
      "key_missing",
      "key_invalid",
      "server_missing",
      "url_invalid",
      "store",
      "disk",
      "unsupported",
      "unpack",
      "network",
      "play_ended",
      "no_session",
      "ffmpeg_missing",
      "no_destinations",
      "already_live",
      "checksum",
      "download",
    ]) {
      expect(problemText(tr, code, "ENGINE ENGLISH", "Twitch"), code).not.toContain("ENGINE ENGLISH");
    }
  });

  it("says HTTP errors in words and passes an unknown code's sentence on", () => {
    expect(problemText(tr, "http_500", "HTTP 500")).toBe("The engine refused that (HTTP 500). Try again.");
    expect(problemText(tr, "brand_new", "A sentence from a newer engine.")).toBe("A sentence from a newer engine.");
  });
});

describe("removing a saved site", () => {
  const tr = ((_k: string, vars?: Record<string, string | number>, fallback?: string) =>
    (fallback ?? _k).replace(/\{(\w+)\}/g, (_, k: string) => String(vars?.[k]))) as Parameters<
    typeof removeConfirmOptions
  >[0];

  it("asks first, saying the stream key goes with it", () => {
    const o = removeConfirmOptions(tr, "My Twitch");
    expect(o.destructive).toBe(true);
    expect(o.title).toBe("Remove My Twitch");
    expect(o.message).toContain("stream key");
    expect(o.confirmLabel).toBe("Remove");
  });

  it("keeps the site and its key when the user says no", async () => {
    const remove = vi.fn();
    expect(await removeAfterConfirm(async () => false, removeConfirmOptions(tr, "X"), remove)).toBe(false);
    expect(remove).not.toHaveBeenCalled();
  });

  it("removes it once the user says yes", async () => {
    const remove = vi.fn();
    expect(await removeAfterConfirm(async () => true, removeConfirmOptions(tr, "X"), remove)).toBe(true);
    expect(remove).toHaveBeenCalledTimes(1);
  });
});
