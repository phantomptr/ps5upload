import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { browserInvoke, BrowserUnsupportedError, parseSseBlock, timeSyncBody } from "./browserInvoke";

vi.mock("../state/engine", () => ({
  getEngineUrl: () => "http://engine.test:19113",
}));

/**
 * The browser transport and the Tauri command are two independent
 * spellings of the same request, and nothing makes them agree
 * automatically — issue #278 was a screen bypassing one of a matched
 * pair. `ps5_time_sync` carries the same hazard: the engine reads
 * snake_case, the TS callers speak camelCase, and a key that is
 * dropped in translation does not error. It silently changes what the
 * console's clock gets set to.
 */
describe("timeSyncBody", () => {
  it("maps a PC-time sync to the engine's snake_case keys", () => {
    expect(
      timeSyncBody({ addr: "10.0.0.5:9113", targetUnixSeconds: 1778887800 }),
    ).toEqual({
      addr: "10.0.0.5:9113",
      target_unix_seconds: 1778887800,
      use_ntp: false,
    });
  });

  it("asks the engine to query NTP when useNtp is set", () => {
    const body = timeSyncBody({ addr: "a", useNtp: true });
    expect(body["use_ntp"]).toBe(true);
  });

  it("does not require a target when syncing from NTP", () => {
    // The whole point of an NTP sync is that the PC clock is not
    // trusted, so the caller has no target to send. The engine
    // defaults the field, but sending a stray 0 would be a
    // 1970 timestamp sitting in a request that sets a console clock.
    const body = timeSyncBody({ addr: "a", useNtp: true });
    expect(body["target_unix_seconds"]).toBeUndefined();
  });

  it("forwards a custom NTP server", () => {
    const body = timeSyncBody({
      addr: "a",
      useNtp: true,
      ntpServer: "time.example.org",
    });
    expect(body["ntp_server"]).toBe("time.example.org");
  });

  it("omits ntp_server when the caller did not pick one", () => {
    // Absent means "use the engine's default list". An explicit null
    // would be a server named null.
    expect("ntp_server" in timeSyncBody({ addr: "a", useNtp: true })).toBe(
      false,
    );
  });
});


/**
 * The install cascade's DPI fallback is the only path that lands a game
 * PATCH: the in-process installer rejects updates on FW 10+, and the
 * ShellUI tier is forbidden for patches because it can wipe the base
 * game. On the desktop the fallback works because the app embeds both
 * ELF images and streams them to the loader itself.
 *
 * A browser can do neither. Before the web UI fix `dpi_ensure` had no entry in
 * this map, so it threw BrowserUnsupportedError, the cascade recorded
 * "daemon failed", and every update installed from the web UI died with
 * the generic "this update couldn't be applied" hint — while base games,
 * which never need the fallback, worked fine. That asymmetry is exactly
 * what users reported.
 */
describe("loader-port commands the DPI fallback depends on", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  function captureFetch(body: unknown) {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        calls.push({ url, init });
        return {
          ok: true,
          status: 200,
          json: async () => body,
        } as unknown as Response;
      }),
    );
    return calls;
  }

  it("routes dpi_ensure to the engine instead of throwing", async () => {
    const calls = captureFetch({ ok: true, listening: true, sent: false });

    const res = await browserInvoke("dpi_ensure", { ip: "192.168.1.50" });

    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("http://engine.test:19113/api/pkg/dpi-ensure");
    // The engine route names the console the same way every other pkg
    // route does; `ip` is the Tauri command's spelling.
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      ps5_addr: "192.168.1.50",
    });
    // `sent` has to survive the translation — the cascade uses it to
    // decide whether the helper was displaced and needs restoring.
    expect(res).toEqual({ ok: true, listening: true, sent: false });
  });

  it("routes payload_restore to the engine", async () => {
    const calls = captureFetch({ ok: true, bytes: 2230616 });

    await browserInvoke("payload_restore", { ip: "192.168.1.50" });

    expect(calls[0].url).toBe(
      "http://engine.test:19113/api/pkg/payload-restore",
    );
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      ps5_addr: "192.168.1.50",
    });
  });

  it("still rejects commands that genuinely have no browser equivalent", async () => {
    // Guard the guard: the fix must not turn the default branch into a
    // silent no-op for desktop-only commands.
    await expect(browserInvoke("payload_bundled_path", {})).rejects.toBeInstanceOf(
      BrowserUnsupportedError,
    );
  });
});

describe("engine-backed commands the web UI depends on (#300)", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  function captureFetch(body: unknown) {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        calls.push({ url, init });
        return { ok: true, status: 200, json: async () => body } as unknown as Response;
      }),
    );
    return calls;
  }

  const B = "http://engine.test:19113";

  it("builds cheats_get with both query params, engine-encoded", async () => {
    const calls = captureFetch({ cheats: [] });
    await browserInvoke("cheats_get", {
      req: { addr: "192.168.1.50:9113", title_id: "CUSA03474" },
    });
    expect(calls[0].url).toBe(
      `${B}/api/ps5/cheats/get?addr=192.168.1.50%3A9113&title_id=CUSA03474`,
    );
  });

  it("omits addr entirely when the caller passes null", async () => {
    // The Rust side pushes no param at all for None. A literal `addr=null`
    // would be parsed as a hostname and fail the connect.
    const calls = captureFetch({ cheats: [] });
    await browserInvoke("cheats_list", { req: { addr: null } });
    expect(calls[0].url).toBe(`${B}/api/ps5/cheats/list`);
  });

  it("keeps since_seq=0 rather than dropping it as falsy", async () => {
    // Dropping it would make the engine replay the whole notification
    // backlog on every poll instead of returning only new rows.
    const calls = captureFetch({ notifications: [] });
    await browserInvoke("notif_list", { req: { addr: null, since_seq: 0 } });
    expect(calls[0].url).toBe(`${B}/api/ps5/notif/list?since_seq=0`);
  });

  it("sends refresh only when it is actually set", async () => {
    // The engine parses `refresh` as a string, so `refresh=false` reads
    // as truthy and would force a cache-busting refetch every time.
    const calls = captureFetch({});
    await browserInvoke("tmdb_fetch", {
      req: { addr: null, title_id: "CUSA03474", refresh: false, region: null },
    });
    expect(calls[0].url).toBe(`${B}/api/ps5/tmdb/fetch?title_id=CUSA03474`);

    await browserInvoke("tmdb_fetch", {
      req: { addr: null, title_id: "CUSA03474", refresh: true, region: null },
    });
    expect(calls[1].url).toBe(
      `${B}/api/ps5/tmdb/fetch?title_id=CUSA03474&refresh=true`,
    );
  });

  it("drops an empty backup tag the way the Rust command does", async () => {
    const calls = captureFetch({ snapshots: [] });
    await browserInvoke("backup_list", { req: { addr: null, tag: "" } });
    expect(calls[0].url).toBe(`${B}/api/ps5/backup/list`);
  });

  it("passes transfer_zip's body through in snake_case", async () => {
    // A key lost in translation here does not throw — it silently changes
    // where a multi-GB install lands, or removes its bandwidth cap.
    const calls = captureFetch({ job_id: "j1" });
    await browserInvoke("transfer_zip", {
      req: {
        zip_path: "/srv/games/x.zip",
        dest_root: "/mnt/usb0",
        addr: "192.168.1.50:9113",
        tx_id: "tx1",
        excludes: ["a"],
        bandwidth_cap_mbps: 200,
      },
    });
    expect(calls[0].url).toBe(`${B}/api/transfer/zip`);
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      zip_path: "/srv/games/x.zip",
      dest_root: "/mnt/usb0",
      addr: "192.168.1.50:9113",
      tx_id: "tx1",
      excludes: ["a"],
      bandwidth_cap_mbps: 200,
    });
  });

  it("maps ffpkg_extract's camelCase args onto the engine's snake_case body", async () => {
    const calls = captureFetch({ ok: true });
    await browserInvoke("ffpkg_extract", {
      ffpkgPath: "/srv/a.ffpkg",
      innerPath: "eboot.bin",
      destDir: "/tmp/out",
    });
    expect(JSON.parse(calls[0].init.body as string)).toEqual({
      ffpkg_path: "/srv/a.ffpkg",
      inner_path: "eboot.bin",
      dest_dir: "/tmp/out",
    });
  });

  it("still refuses commands that write to a host path", async () => {
    // smb_download_file saves bytes to the desktop's disk; mapping it
    // would silently write onto the ENGINE's filesystem instead.
    await expect(
      browserInvoke("smb_download_file", { req: {} }),
    ).rejects.toBeInstanceOf(BrowserUnsupportedError);
  });
});

describe("streaming archive inspect over SSE", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  /** Serve a fixed SSE body as a ReadableStream, split across chunks so the
   *  parser's cross-chunk buffering is actually exercised. */
  function stubSse(chunks: string[], ok = true) {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok,
        status: ok ? 200 : 500,
        json: async () => ({ error: "bad request" }),
        text: async () => "bad request",
        body: {
          getReader() {
            let i = 0;
            return {
              read: async () =>
                i < chunks.length
                  ? { done: false, value: new TextEncoder().encode(chunks[i++]) }
                  : { done: true, value: undefined },
              cancel: async () => {},
            };
          },
        },
      })) as unknown as typeof fetch,
    );
  }

  it("forwards progress ticks and resolves with the done payload", async () => {
    stubSse([
      'event: progress\ndata: {"entries_seen":10}\n\nevent: prog',
      'ress\ndata: {"entries_seen":25}\n\n',
      'event: done\ndata: {"entries":25,"total_bytes":99}\n\n',
    ]);
    const seen: unknown[] = [];
    const res = await browserInvoke("zip_inspect_stream", {
      req: { zip_path: "/srv/a.zip" },
      onProgress: { onmessage: (p: unknown) => seen.push(p) },
    });
    // The second event was split mid-word across two chunks; if the
    // buffer were per-chunk it would be lost.
    expect(seen).toEqual([{ entries_seen: 10 }, { entries_seen: 25 }]);
    expect(res).toEqual({ entries: 25, total_bytes: 99 });
  });

  it("surfaces an SSE error event as a rejection", async () => {
    stubSse(['event: error\ndata: {"error":"unreadable archive"}\n\n']);
    await expect(
      browserInvoke("sevenz_inspect_stream", {
        req: { archive_path: "/srv/a.7z" },
        onProgress: { onmessage: () => {} },
      }),
    ).rejects.toThrow(/unreadable archive/);
  });

  it("fails loudly if the stream ends without done or error", async () => {
    stubSse(['event: progress\ndata: {"entries_seen":1}\n\n']);
    await expect(
      browserInvoke("zip_inspect_stream", {
        req: { zip_path: "/srv/a.zip" },
        onProgress: { onmessage: () => {} },
      }),
    ).rejects.toThrow(/closed the inspect stream/);
  });

  it("keeps going when the progress callback throws", async () => {
    // Matches the Rust Channel::send, which is fire-and-forget: an
    // unmounted component must not abort a succeeding inspect.
    stubSse([
      'event: progress\ndata: {"entries_seen":1}\n\n',
      'event: done\ndata: {"entries":1}\n\n',
    ]);
    const res = await browserInvoke("zip_inspect_stream", {
      req: { zip_path: "/srv/a.zip" },
      onProgress: {
        onmessage: () => {
          throw new Error("component unmounted");
        },
      },
    });
    expect(res).toEqual({ entries: 1 });
  });

  it("parses SSE blocks the way the Rust parser does", () => {
    expect(parseSseBlock("event: done\ndata: {}")).toEqual({
      event: "done",
      data: "{}",
    });
    // Multi-line data joins with newlines; comments and CR are ignored.
    expect(parseSseBlock(": keepalive\r\nevent: progress\r\ndata: a\r\ndata: b")).toEqual(
      { event: "progress", data: "a\nb" },
    );
    expect(parseSseBlock(": just a comment")).toBeNull();
  });
});
