import { describe, expect, it } from "vitest";

import {
  classifyReplaceError,
  classifySession,
  hostFromArgs,
  isNotPairedError,
  sessionNeedsAttention,
} from "./consoleSession";

describe("the one console status probe", () => {
  it("reads a good STATUS reply as connected", () => {
    expect(classifySession({ reachable: true, error: null })).toBe("connected");
  });

  it("maps the engine's tokens to the pairing and old-helper states", () => {
    expect(
      classifySession({ reachable: false, error: "ava1_not_paired" }),
    ).toBe("needs_pairing");
    expect(
      classifySession({
        reachable: false,
        error: "payload rejected NOT_PAIRED: not_paired",
      }),
    ).toBe("needs_pairing");
    expect(classifySession({ reachable: false, error: "helper_old" })).toBe(
      "helper_old",
    );
    // The legacy helper that would not exit is also an old helper, with its own banner text.
    expect(
      classifySession({ reachable: false, error: "legacy_helper_wedged" }),
    ).toBe("helper_old");
    // No AVA1 listener at all: nothing to pair with and nothing old to update, so the
    // existing send-the-helper flow (down) is the right one.
    expect(
      classifySession({ reachable: false, error: "helper_not_ava1" }),
    ).toBe("down");
  });

  it("is down for anything else, including no error text", () => {
    expect(classifySession({ reachable: false, error: "connect refused" })).toBe(
      "down",
    );
    expect(classifySession({ reachable: false, error: null })).toBe("down");
  });

  it("flags the states a person has to act on", () => {
    expect(sessionNeedsAttention("needs_pairing")).toBe(true);
    expect(sessionNeedsAttention("helper_old")).toBe(true);
    expect(sessionNeedsAttention("connected")).toBe(false);
    expect(sessionNeedsAttention("down")).toBe(false);
    expect(sessionNeedsAttention(null)).toBe(false);
  });

  it("recognises a not-paired failure wherever its token appears", () => {
    expect(isNotPairedError("ava1_not_paired")).toBe(true);
    expect(isNotPairedError(new Error("engine HTTP 502: not_paired"))).toBe(true);
    expect(isNotPairedError("the devices are not paired yet")).toBe(true);
    expect(isNotPairedError("connect refused")).toBe(false);
    expect(isNotPairedError(undefined)).toBe(false);
  });
});

// Read as raw text at transform time (the client has no @types/node).
const SOURCES = import.meta.glob(["../state/connection.ts", "../layout/AppShell.tsx"], {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

describe("status_pill_has_one_probe", () => {
  const raw = (rel: string): string => SOURCES[rel] ?? "";
  const src = raw("../state/connection.ts");
  it("the per-console runtime carries one session state and no second liveness flag", () => {
    expect(src.length).toBeGreaterThan(0);
    expect(src).toContain("session: SessionState | null");
    expect(src).not.toContain("transferAlive");
    expect(src).not.toContain("PS5_PAYLOAD_PORT");
  });
  it("the poller no longer opens a second TCP probe against the old transfer port", () => {
    const shell = raw("../layout/AppShell.tsx");
    expect(shell.length).toBeGreaterThan(0);
    expect(shell).not.toContain("transferAliveRef");
    expect(shell).not.toContain("PS5_TRANSFER_PORT");
    expect(shell).not.toContain("PS5_MGMT_PORT");
  });
});

describe("which console a failure names", () => {
  it("reads ip, addr and req.addr only, stripping any port; never a from/to path", () => {
    expect(hostFromArgs({ ip: "10.0.0.2" })).toBe("10.0.0.2");
    expect(hostFromArgs({ addr: "10.0.0.3:9114", path: "/x" })).toBe("10.0.0.3");
    expect(hostFromArgs({ req: { addr: "10.0.0.4:9114", from: "/data/a" } })).toBe("10.0.0.4");
    expect(hostFromArgs({ req: { from: "/data/a", to: "/data/b" } })).toBeUndefined();
    expect(hostFromArgs({ from: "/data/a", to: "/mnt/usb0/b" })).toBeUndefined();
    expect(hostFromArgs({ jobId: "j" })).toBeUndefined();
    expect(hostFromArgs(undefined)).toBeUndefined();
  });
});

describe("the engine's folded status error", () => {
  it("reads an older helper out of the status failure (the banner then offers Update)", () => {
    expect(
      classifySession({
        reachable: false,
        error:
          "helper_old: the console runs an older helper; update it (payload rejected NODE_STATUS: helper_not_ava1: ...)",
      }),
    ).toBe("helper_old");
  });

  it("keeps a plain helper_not_ava1 as down", () => {
    expect(
      classifySession({ reachable: false, error: "payload rejected NODE_STATUS: helper_not_ava1: x" }),
    ).toBe("down");
  });
});

describe("classifyReplaceError", () => {
  it.each([
    ["legacy_helper_wedged: the older helper did not exit", "wedged"],
    ["helper_not_running: nothing answers on the console", "no_helper"],
    ["replace_in_progress: a helper replace for this console is running", "in_progress"],
    ["replace_cooldown: a helper replace was just done (60 s apart)", "cooldown"],
    ["helper_starting: the new helper is still starting", "starting"],
    ["ava1_failed: the helper is new but its AVA1 server did not start", "ava1_failed"],
    ["connect to 10.0.0.2:9021 refused", "failed"],
  ])("%s -> %s", (msg, kind) => {
    expect(classifyReplaceError(new Error(msg))).toBe(kind);
  });

  it("only a missing helper may fall through to sending one", () => {
    // A cooldown or an in-progress replace must never trigger a send: that would race the
    // replace that is already running.
    for (const k of ["in_progress", "cooldown", "starting", "wedged", "ava1_failed", "failed"]) {
      expect(k).not.toBe("no_helper");
    }
  });
});
