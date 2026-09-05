import { afterEach, describe, expect, it, vi } from "vitest";

import { browserInvoke, BrowserUnsupportedError, timeSyncBody } from "./browserInvoke";

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
 * A browser can do neither. Before #295 `dpi_ensure` had no entry in
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
