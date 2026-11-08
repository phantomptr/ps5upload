import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  browserInvoke,
  BrowserUnsupportedError,
  parseSseBlock,
  statusFailureFields,
  statusFailureText,
  timeSyncBody,
} from "./browserInvoke";

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
describe("an engine error", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("reports a plain-text body, not a bare status code", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response("Failed to parse the request body", { status: 422 })));
    await expect(browserInvoke("ps5_volumes", { addr: "a" })).rejects.toThrow("Failed to parse the request body");
  });

  it("reports the JSON error, and the status when the body is empty", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ error: "busy" }), { status: 502 })));
    await expect(browserInvoke("ps5_volumes", { addr: "a" })).rejects.toThrow("busy");
    vi.stubGlobal("fetch", vi.fn(async () => new Response("", { status: 503 })));
    await expect(browserInvoke("ps5_volumes", { addr: "a" })).rejects.toThrow("engine HTTP 503");
  });
});

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

describe("ShadowMount+ image read-write commands", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("maps status, begin, and finish to the engine API", async () => {
    const calls: Array<{ url: string; init?: RequestInit }> = [];
    vi.stubGlobal("fetch", vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      const body = url.includes("/image-rw?") ? { session: null } : { title_id: "PPSA30528" };
      return { ok: true, status: 200, json: async () => body } as Response;
    }));

    expect(await browserInvoke("smp_image_rw_status", { addr: "10.0.0.2:9114" })).toBeNull();
    await browserInvoke("smp_image_rw_begin", { addr: "10.0.0.2:9114", titleId: "PPSA30528" });
    await browserInvoke("smp_image_rw_finish", { addr: "10.0.0.2:9114" });

    expect(calls.map((c) => c.url)).toEqual([
      "http://engine.test:19113/api/ps5/smp/image-rw?addr=10.0.0.2%3A9114",
      "http://engine.test:19113/api/ps5/smp/image-rw/begin",
      "http://engine.test:19113/api/ps5/smp/image-rw/finish",
    ]);
    expect(JSON.parse(calls[1].init?.body as string)).toEqual({
      addr: "10.0.0.2:9114",
      title_id: "PPSA30528",
    });
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

  it("routes the unified pkg_install to POST /api/pkg/install with the req body", async () => {
    const calls = captureFetch({ ok: true, job: "17-1" });
    const req = { ps5_addr: "192.168.1.50:9114", source: { console_path: "/user/data/x.pkg" } };
    const res = await browserInvoke("pkg_install", { req });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("http://engine.test:19113/api/pkg/install");
    expect(JSON.parse(calls[0].init.body as string)).toEqual(req);
    expect(res).toEqual({ ok: true, job: "17-1" });
  });

  it("routes pkg_install_status_v2 to the job-based status", async () => {
    const calls = captureFetch({ ok: true, phase: "done", verdict: "installed" });
    await browserInvoke("pkg_install_status_v2", { job: "17-1" });
    expect(calls[0].url).toBe("http://engine.test:19113/api/pkg/install/status?job=17-1");
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

  it("cuts a page of the engine's whole local listing, as the app's command does", async () => {
    const all = [{ name: "a" }, { name: "b" }, { name: "c" }];
    const calls = captureFetch(all);
    await expect(browserInvoke("local_list_dir", { path: "/d", offset: 1, limit: 1 })).resolves.toEqual([
      { name: "b" },
    ]);
    await expect(browserInvoke("local_list_dir", { path: "/d" })).resolves.toEqual(all);
    expect(calls[0].url).toBe(`${B}/api/local/list-dir?path=%2Fd`);
  });

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

  it("sends Remote Play cancel's console in the query, like the desktop app", async () => {
    // A body-only addr reached the default console on engines that read the query alone:
    // cancel timed out and the real console kept its PIN.
    const calls = captureFetch({ ok: true });
    await browserInvoke("remoteplay_cancel", { addr: "192.0.2.99" });
    expect(calls[0].url).toBe(`${B}/api/ps5/remoteplay/cancel?addr=192.0.2.99`);
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ addr: "192.0.2.99" });
  });

  it("asks the engine about the console's elfldr", async () => {
    const calls = captureFetch({ health: "healthy" });
    await browserInvoke("ps5_elfldr_health", { host: "10.0.0.2" });
    expect(calls[0].url).toBe(`${B}/api/ps5/elfldr/health?host=10.0.0.2`);
    await browserInvoke("ps5_elfldr_ensure", { host: "10.0.0.2" });
    expect(calls[1].url).toBe(`${B}/api/ps5/elfldr/ensure`);
    expect(JSON.parse(String(calls[1].init.body))).toEqual({ host: "10.0.0.2" });
  });

  it("asks the engine to load a payload that is on the console", async () => {
    const calls = captureFetch({ bytes: 1 });
    await browserInvoke("ps5_payload_load_console", { host: "10.0.0.2", path: "/data/k.elf" });
    expect(calls[0].url).toBe(`${B}/api/ps5/payload/load-console`);
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ host: "10.0.0.2", path: "/data/k.elf" });
  });

  it("tells the estimate where the package would go", async () => {
    const calls = captureFetch({});
    await browserInvoke("fpkg_estimate", { source: "/g", outputDir: "/out" });
    expect(calls[0].url).toBe(`${B}/api/fpkg/estimate`);
    expect(JSON.parse(String(calls[0].init.body))).toEqual({ source: "/g", output_dir: "/out" });
  });

  it("sends a Convert build's language to the engine", async () => {
    const calls = captureFetch({ job_id: "j" });
    await browserInvoke("fpkg_build", { source: "/g", language: "de-DE" });
    expect(calls[0].url).toBe(`${B}/api/fpkg/build`);
    expect(JSON.parse(String(calls[0].init.body))).toMatchObject({ source: "/g", language: "de-DE" });
  });

  it("sends the PS4 update a package build merges into its game", async () => {
    const calls = captureFetch({ job_id: "j" });
    await browserInvoke("fpkg_build", { source: "/game.pkg", update: "/update.pkg" });
    expect(JSON.parse(String(calls[0].init.body))).toMatchObject({ source: "/game.pkg", update: "/update.pkg" });
  });

  it("keeps since_seq=0 rather than dropping it as falsy", async () => {
    // Dropping it would make the engine replay the whole notification
    // backlog on every poll instead of returning only new rows.
    const calls = captureFetch({ notifications: [] });
    await browserInvoke("notif_list", { req: { addr: null, since_seq: 0 } });
    expect(calls[0].url).toBe(`${B}/api/ps5/notif/list?since_seq=0`);
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

  it("sends transfer_file_list to the engine's file-list job, body unchanged", async () => {
    // The Files screen's multi-file pick: one job for the set. The web UI must reach the same
    // route the desktop shell proxies to, or a pick of thousands of files breaks there.
    const calls = captureFetch({ job_id: "j2" });
    const req = {
      dest_root: "/data/x",
      files: [
        { src: "/srv/a.bin", dest: "/data/x/a.bin" },
        { src: "/srv/b.bin", dest: "/data/x/b (2).bin" },
      ],
      addr: "192.168.1.50",
      tx_id: "0123456789abcdef0123456789abcdef",
      bandwidth_cap_mbps: null,
    };
    const r = await browserInvoke<{ job_id: string }>("transfer_file_list", { req });
    expect(r.job_id).toBe("j2");
    expect(calls[0].url).toBe(`${B}/api/transfer/file-list`);
    expect(calls[0].init.method).toBe("POST");
    expect(JSON.parse(calls[0].init.body as string)).toEqual(req);
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
    // screenshot_save writes to the desktop's own screenshot folder; mapping
    // it would silently write onto the ENGINE's filesystem instead.
    await expect(
      browserInvoke("screenshot_save", { req: {} }),
    ).rejects.toBeInstanceOf(BrowserUnsupportedError);
  });
});

describe("Payload Library and console tools in the web UI", () => {
  const fetchMock = vi.fn();
  beforeEach(() => {
    fetchMock.mockReset();
    vi.stubGlobal("fetch", fetchMock);
  });
  afterEach(() => vi.unstubAllGlobals());

  const answer = (body: unknown, status = 200) =>
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify(body), { status }),
    );
  const asked = () => {
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit | undefined];
    return {
      url: url.replace("http://engine.test:19113", ""),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(init.body as string) : undefined,
    };
  };

  it("reads the catalogue and the cache from one route", async () => {
    answer({ payloads: [{ id: "kstuff" }], local: [{ payload_id: "kstuff", path: "/d/k.elf" }] });
    expect(await browserInvoke("payloads_catalog", {})).toEqual([{ id: "kstuff" }]);
    answer({ payloads: [], local: [{ payload_id: "kstuff", path: "/d/k.elf" }] });
    expect(await browserInvoke("payloads_local_inventory", {})).toEqual([
      { payload_id: "kstuff", path: "/d/k.elf" },
    ]);
    answer({ payloads: [], local: [{ payload_id: "kstuff", path: "/d/k.elf" }] });
    expect(await browserInvoke("payloads_local_path", { id: "kstuff" })).toBe("/d/k.elf");
    answer({ payloads: [], local: [] });
    expect(await browserInvoke("payloads_local_path", { id: "kstuff" })).toBeNull();
  });

  it("asks for releases, forced only when asked", async () => {
    answer([]);
    await browserInvoke("payloads_releases", { id: "a b", forceRefresh: false });
    expect(asked().url).toBe("/api/payloads/a%20b/releases");
    fetchMock.mockReset();
    answer({ tag: "v1" });
    await browserInvoke("payloads_release", { id: "k", forceRefresh: true });
    expect(asked().url).toBe("/api/payloads/k/releases/latest?force=1");
  });

  it("downloads and cancels by id", async () => {
    answer({ payload_id: "k" });
    await browserInvoke("payloads_download", { id: "k", assetUrl: "https://x/a.elf", version: "v1" });
    expect(asked()).toEqual({
      url: "/api/payloads/k/download",
      method: "POST",
      body: { asset_url: "https://x/a.elf", version: "v1" },
    });
    fetchMock.mockReset();
    answer({ cancelled: true });
    expect(await browserInvoke("payloads_download_cancel", { id: "k" })).toBe(true);
    expect(asked().url).toBe("/api/payloads/k/download/cancel");
  });

  it("adds and removes a repo", async () => {
    answer({ id: "custom-0a" });
    await browserInvoke("payloads_add_custom_repo", {
      host: "github.com",
      owner: "o",
      name: "n",
      displayName: null,
      assetHint: null,
      checksumFile: "SHA256SUMS",
    });
    expect(asked().body).toEqual({
      host: "github.com",
      owner: "o",
      name: "n",
      display_name: null,
      asset_hint: null,
      checksum_file: "SHA256SUMS",
    });
    fetchMock.mockReset();
    answer({ removed: true });
    expect(await browserInvoke("payloads_remove_custom_repo", { id: "custom-0a" })).toBe(true);
    expect(asked()).toMatchObject({ url: "/api/payloads/repos/custom-0a", method: "DELETE" });
  });

  it("keeps a worded code in front of the engine's error, and only that", async () => {
    answer({ error: "1.2.3.4 is an IP", code: "repo_host_is_ip" }, 400);
    await expect(
      browserInvoke("payloads_add_custom_repo", { host: "1.2.3.4", owner: "o", name: "n" }),
    ).rejects.toThrow("repo_host_is_ip: 1.2.3.4 is an IP");
    answer({ error: "fetch x: HTTP 404 (Not Found)", code: "fetch_failed" }, 502);
    await expect(browserInvoke("payloads_release", { id: "k" })).rejects.toThrow(
      /^fetch x: HTTP 404/,
    );
  });

  it("sends a file by path, with the port only when one is given", async () => {
    answer({ ok: true, status: "sent 5 bytes", bytes: 5 });
    await browserInvoke("payload_send", { ip: "10.0.0.2", path: "/d/k.elf", port: null });
    expect(asked().body).toEqual({ host: "10.0.0.2", path: "/d/k.elf" });
  });

  it("turns the desktop's arguments into the console routes", async () => {
    answer({ exit_code: 0 });
    await browserInvoke("shell_run_cmd", {
      addr: "10.0.0.2",
      cmd: "ls",
      sessionId: "s",
      cwd: "/data",
      timeoutSecs: 5,
    });
    expect(asked().body).toEqual({
      addr: "10.0.0.2",
      cmd: "ls",
      session_id: "s",
      cwd: "/data",
      timeout_s: 5,
    });

    fetchMock.mockReset();
    answer({ modules: [] });
    await browserInvoke("proc_modules_get", { addr: "10.0.0.2", pid: 77 });
    expect(asked().url).toBe("/api/ps5/process/77/modules?addr=10.0.0.2");

    fetchMock.mockReset();
    answer({ apps: [] });
    await browserInvoke("appdb_query_get", { addr: "10.0.0.2" });
    expect(asked().url).toBe("/api/ps5/appdb?addr=10.0.0.2");

    fetchMock.mockReset();
    answer({ hash: "x" });
    await browserInvoke("fs_blake3_hash", { addr: "a", path: "/f" });
    expect(asked().body).toEqual({ addr: "a", path: "/f", algo: "blake3" });

    fetchMock.mockReset();
    answer({ crc32: 1 });
    await browserInvoke("crc32_file_get", { addr: "a", path: "/f" });
    expect(asked().body).toEqual({ addr: "a", path: "/f", algo: "crc32" });

    fetchMock.mockReset();
    answer({ copied: 1 });
    await browserInvoke("heal_appmeta", { addr: "a", titleId: "CUSA00001", sourcePath: "/g" });
    expect(asked().body).toEqual({ addr: "a", title_id: "CUSA00001", source_path: "/g" });

    fetchMock.mockReset();
    answer([]);
    await browserInvoke("companion_probe", { host: "10.0.0.2" });
    expect(asked().url).toBe("/api/ps5/companions?addr=10.0.0.2");

    fetchMock.mockReset();
    answer({ ok: true, candidates: [] });
    await browserInvoke("discover_ps5", { timeoutSecs: 4 });
    expect(asked()).toMatchObject({ url: "/api/discover", body: { timeout_ms: 4000 } });
    fetchMock.mockReset();
    answer({ ok: true, candidates: [] });
    await browserInvoke("discover_ps5", { timeoutSecs: null });
    expect(asked().body).toEqual({});
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

/**
 * `payload_check` is the status probe the poller reads rest mode from: the engine's 502 body
 * `{ error, reason, power }` must reach the caller the same way probes.rs passes it through.
 */
describe("payload_check passes the status failure's reason and power through", () => {
  afterEach(() => vi.unstubAllGlobals());

  function stubStatus(status: number, body: string) {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: status >= 200 && status < 300,
        status,
        text: async () => body,
        json: async () => JSON.parse(body) as unknown,
      })) as unknown as typeof fetch,
    );
  }

  it("parses reason and power from a rest-mode reply", async () => {
    const body = JSON.stringify({
      error: "payload rejected NODE_STATUS: console_rest_mode: The PS5 is in rest mode.",
      reason: "console_rest_mode",
      power: "rest",
    });
    stubStatus(502, body);
    const res = await browserInvoke<Record<string, unknown>>("payload_check", {
      ip: "10.0.0.2",
    });
    expect(res).toEqual({
      ok: false,
      reachable: false,
      engine: true,
      error: body,
      reason: "console_rest_mode",
      power: "rest",
    });
  });

  it("leaves both out for an older engine's body or a non-JSON one", async () => {
    stubStatus(502, JSON.stringify({ error: "helper_not_ava1: gone" }));
    const a = await browserInvoke<Record<string, unknown>>("payload_check", {
      ip: "10.0.0.2",
    });
    expect("reason" in a || "power" in a).toBe(false);
    stubStatus(502, "Bad Gateway");
    const b = await browserInvoke<Record<string, unknown>>("payload_check", {
      ip: "10.0.0.2",
    });
    expect(b["error"]).toBe("Bad Gateway");
    expect("reason" in b || "power" in b).toBe(false);
  });

  it("statusFailureFields keeps only string fields", () => {
    expect(
      statusFailureFields('{"reason":"ava1_unreachable","power":"unreachable"}'),
    ).toEqual({ reason: "ava1_unreachable", power: "unreachable" });
    expect(statusFailureFields('{"reason":5,"power":null}')).toEqual({});
    expect(statusFailureFields("null")).toEqual({});
    expect(statusFailureFields("")).toEqual({});
  });
});


describe("remote conversion preflight", () => {
  afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });

  it.each(["fpkg_inspect", "fpkg_build", "fpkg_estimate"])("%s allows console metadata reads beyond one minute", async (cmd) => {
    const deadline = vi.spyOn(AbortSignal, "timeout");
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ files: 42 }), { status: 200 })));
    await browserInvoke(cmd, { source: "ps5://console/data/game.exfat", outputDir: "/games" });
    expect(deadline).toHaveBeenCalledWith(3_600_000);
    const call = vi.mocked(fetch).mock.calls[0];
    expect(JSON.parse(call[1]?.body as string).source).toBe("ps5://console/data/game.exfat");
  });
});

describe("engine errors", () => {
  const realFetch = globalThis.fetch;
  afterEach(() => {
    globalThis.fetch = realFetch;
  });

  it("status failures read a normalized refusal as its text and keep other bodies raw", () => {
    expect(statusFailureText('{"error":"loopback only","code":"forbidden"}')).toBe("loopback only");
    expect(statusFailureText("  plain text ")).toBe("plain text");
    const withReason = '{"error":"x","reason":"console_rest_mode"}';
    expect(statusFailureText(withReason)).toBe(withReason);
    expect(statusFailureText("")).toBe("");
  });

  it("a failed call throws the engine's text, from JSON or from a plain body", async () => {
    const answer = (body: string, status: number) => {
      globalThis.fetch = vi.fn(async () => new Response(body, { status })) as typeof fetch;
    };
    answer('{"error":"Missing field `path`","code":"unprocessable_entity"}', 422);
    await expect(browserInvoke("inspect_folder", { path: "/x" })).rejects.toThrow("Missing field `path`");
    // The console refused a drive action: a 502 now, read like any other failure.
    answer('{"error":"PERIPHERAL_CONTROL failed: code 3","reason":"helper_refused"}', 502);
    await expect(browserInvoke("peripheral_eject", { addr: "10.0.0.2" })).rejects.toThrow(
      "PERIPHERAL_CONTROL failed",
    );
    answer("a proxy said no", 502);
    await expect(browserInvoke("inspect_folder", { path: "/x" })).rejects.toThrow("a proxy said no");
    answer("", 500);
    await expect(browserInvoke("inspect_folder", { path: "/x" })).rejects.toThrow("engine HTTP 500");
  });
});
