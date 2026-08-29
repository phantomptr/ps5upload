import { describe, expect, it } from "vitest";

import { timeSyncBody } from "./browserInvoke";

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
