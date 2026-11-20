import { beforeEach, describe, expect, it } from "vitest";

// The 12.6.15 decisions behind every helper send, as tables: the shared "loader first seen open"
// record, the cooldown as the send starts, which failures count as a send, which answers mean a
// helper is running, what the reconnect loop says when a send held back, and the console's own
// busy report.

import { parseHelperBusy } from "../api/ps5";
import {
  checkShowsLiveHelper,
  helperAnsweredButUnusable,
  LOADER_SETTLE_MS,
  redeployWhyFromOutcome,
} from "./autoRedeploy";
import { replaceHold } from "./ensurePayloadCurrent";
import {
  consoleKey,
  RESEND_COOLDOWN_MS,
  resetHelperSendGate,
  sendFailedPastConnect,
  sendHelperOnce,
  sentAgoMs,
} from "./helperSendGate";
import {
  justStarted,
  loaderOpenSince,
  noteConsoleGone,
  noteHelperUp,
  noteLoader,
  resetLoaderSettle,
  settleDecision,
  unattendedSettle,
} from "./loaderSettle";

const H = "192.168.1.20";

describe("the loader first seen open: one record for every send", () => {
  beforeEach(() => resetLoaderSettle());

  it.each([
    // loaderOpen, firstOpenAt, helperUpAt, now, want
    [false, 0, undefined, 1_000_000, "closed"],
    [true, undefined, undefined, 1_000_000, "settle"], // knowing nothing: taken as just opened
    [true, 1_000_000 - LOADER_SETTLE_MS + 1, undefined, 1_000_000, "settle"],
    [true, 1_000_000 - LOADER_SETTLE_MS, undefined, 1_000_000, "send"],
    [true, undefined, 999_000, 1_000_000, "send"], // our helper answered: the console is not starting
  ] as const)("open=%s first=%s helperUp=%s at %s: %s", (loaderOpen, firstOpenAt, helperUpAt, now, want) => {
    expect(settleDecision({ loaderOpen, firstOpenAt, helperUpAt, now })).toBe(want);
  });

  it("a console seen gone starts over, and the wait runs from when its loader came back", () => {
    noteHelperUp(H, 0);
    expect(unattendedSettle(H, true, LOADER_SETTLE_MS, 10).state).toBe("send");
    noteConsoleGone(H);
    expect(loaderOpenSince(H)).toBeUndefined();
    const s = unattendedSettle(H, true, LOADER_SETTLE_MS, 100_000);
    expect(s).toEqual({ state: "settle", sendAt: 100_000 + LOADER_SETTLE_MS });
    // Every path reads the same record: a later look keeps the first sighting.
    expect(unattendedSettle(`${H}:9021`, true, LOADER_SETTLE_MS, 150_000).sendAt).toBe(100_000 + LOADER_SETTLE_MS);
    expect(unattendedSettle(H, true, LOADER_SETTLE_MS, 100_000 + LOADER_SETTLE_MS).state).toBe("send");
  });

  it("a closed loader is a console that went: its next open is a start", () => {
    noteLoader(H, true, 0);
    noteLoader(H, false, 10);
    expect(loaderOpenSince(H)).toBeUndefined();
    noteLoader(H, true, 20);
    expect(loaderOpenSince(H)).toBe(20);
  });

  it("someone's send is told 'just started' only for a start this app saw", () => {
    // Never seen go: the console may have been up for hours.
    noteLoader(H, true, 1_000);
    expect(justStarted(H, LOADER_SETTLE_MS, 2_000)).toBeNull();
    // Seen go, then back moments ago.
    noteConsoleGone(H);
    noteLoader(H, true, 10_000);
    expect(justStarted(H, LOADER_SETTLE_MS, 20_000)).toEqual({ sendAt: 10_000 + LOADER_SETTLE_MS });
    expect(justStarted(H, LOADER_SETTLE_MS, 10_000 + LOADER_SETTLE_MS)).toBeNull();
    // Our helper answering since means it has finished starting.
    noteHelperUp(H, 30_000);
    expect(justStarted(H, LOADER_SETTLE_MS, 31_000)).toBeNull();
  });
});

describe("the cooldown is checked as the send starts", () => {
  beforeEach(() => resetHelperSendGate());

  it("a send within the window gets none, decided in the same step", async () => {
    let t = 1_000_000;
    const now = () => t;
    let sends = 0;
    const send = async () => void sends++;
    expect(await sendHelperOnce(H, send, { minGapMs: RESEND_COOLDOWN_MS, now })).toBe("sent");
    t += RESEND_COOLDOWN_MS - 1;
    expect(await sendHelperOnce(H, send, { minGapMs: RESEND_COOLDOWN_MS, now })).toBe("recent");
    t += 1;
    expect(await sendHelperOnce(H, send, { minGapMs: RESEND_COOLDOWN_MS, now })).toBe("sent");
    expect(sends).toBe(2);
  });

  it("two callers that both read 'nothing sent lately' send one helper", async () => {
    let sends = 0;
    const send = () => new Promise<void>((r) => setTimeout(() => (sends++, r()), 5));
    const [a, b] = await Promise.all([
      sendHelperOnce(H, send, { minGapMs: RESEND_COOLDOWN_MS }),
      sendHelperOnce(`${H}:9113`, send, { minGapMs: RESEND_COOLDOWN_MS }),
    ]);
    expect([a, b].sort()).toEqual(["joined", "sent"]);
    expect(sends).toBe(1);
    expect(sentAgoMs(H)).not.toBeNull();
  });

  it("every table keys a console the same way", () => {
    expect(consoleKey(" 192.168.1.20:9021 ")).toBe(consoleKey("192.168.1.20"));
    expect(consoleKey("PS5.LAN")).toBe("ps5.lan");
  });

  it.each([
    ["send timed out: 10.0.0.5:9021 took nothing for 20 s", true],
    ["write 10.0.0.5:9021: Broken pipe (os error 32)", true],
    ["shutdown: Socket is not connected", true],
    ["connect 10.0.0.5:9021: Connection refused (os error 61)", false],
    ['cannot resolve "ps5.lan": no addresses', false],
    ["this engine build has no ps5upload payload bundled", false],
    ["the engine has no helper to send", false],
  ] as const)("%s counts as a send: %s", (msg, want) => {
    expect(sendFailedPastConnect(msg)).toBe(want);
  });
});

describe("a helper that answered is running", () => {
  it.each([
    [{ reachable: false, reason: "helper_alive_unreadable" }, true],
    [{ reachable: false, reason: "helper_too_new" }, true],
    [{ reachable: false, reason: "helper_not_ava1", helperTooOld: true }, true],
    [{ reachable: false, reason: "ava1_unreachable", helperAlive: true }, true],
    [{ reachable: false, reason: "helper_not_ava1" }, false],
    [{ reachable: false, reason: "ava1_unreachable" }, false],
    [{ reachable: false, reason: "console_rest_mode" }, false],
  ] as const)("%j: %s", (check, want) => {
    expect(helperAnsweredButUnusable(check)).toBe(want);
    expect(checkShowsLiveHelper(check)).toBe(want);
  });
});

describe("why an unattended send held back, on the screen", () => {
  it.each([
    [{ why: "send_budget_spent", retryAt: 5 }, { kind: "limit", code: "send_budget_spent", retryAt: 5 }],
    [{ why: "console_restarted_after_sends" }, { kind: "limit", code: "console_restarted_after_sends", retryAt: null }],
    [{ why: "different_console" }, { kind: "limit", code: "different_console", retryAt: null }],
    [{ why: "console_busy" }, { kind: "console-busy" }],
    [{ why: "console-busy" }, { kind: "console-busy" }],
    [{ why: "others-busy" }, { kind: "busy" }],
    [{ why: "stale-build" }, { kind: "stale-build" }],
    [{ why: "helper_version_mismatch" }, { kind: "stale-build" }],
    [{ why: "answering" }, { kind: "answering" }],
    [{ why: "settling", retryAt: 90_000 }, { kind: "settling", sendAt: 90_000 }],
    [{ why: "loader-closed" }, { kind: "loader-closed" }],
    [{ why: "recent" }, null],
    [null, null],
  ] as const)("%j", (o, want) => {
    expect(redeployWhyFromOutcome(o)).toEqual(want);
  });
});

describe("replacing a running helper unasked", () => {
  const base = {
    running: "12.6.14",
    want: "12.6.15",
    othersBusy: false,
    filesBusy: false,
    busy: parseHelperBusy({ uploads: 0, idle: true }),
    helperAnswers: true,
    mode: "auto" as const,
    consoleChecked: false,
  };
  it.each([
    [{}, null],
    [{ running: "12.7.0" }, "newer"],
    [{ othersBusy: true }, "others-busy"],
    [{ filesBusy: true }, "files-busy"],
    [{ busy: parseHelperBusy({ install: 1 }) }, "console-busy"],
    [{ busy: null }, "console-unknown"],
    [{ busy: null, consoleChecked: true }, null],
    [{ busy: null, mode: "explicit" as const }, null],
    // A port that takes connections but no helper answering: only the device and Files checks.
    [{ busy: null, helperAnswers: false, running: null }, null],
  ] as const)("%j: %s", (over, want) => {
    expect(replaceHold({ ...base, ...over })?.why ?? null).toBe(want);
  });
});

describe("the console's own busy report", () => {
  it("reads every field, and an older helper (no object) is unknown", () => {
    expect(parseHelperBusy(undefined)).toBeNull();
    expect(parseHelperBusy([1])).toBeNull();
    expect(parseHelperBusy({ uploads: 0, other_devices: 2 })).toEqual({
      uploads: 0,
      install: false,
      extract: false,
      convert: false,
      dump: false,
      input: false,
      pads: 0,
      otherDevices: 2,
      idle: true,
    });
    for (const k of ["install", "extract", "convert", "dump", "input"]) {
      expect(parseHelperBusy({ [k]: 1 })?.idle, k).toBe(false);
    }
    expect(parseHelperBusy({ uploads: 3 })?.idle).toBe(false);
    expect(parseHelperBusy({ pads: 1 })?.idle).toBe(false);
    // Other devices merely connected are not work.
    expect(parseHelperBusy({ other_devices: 5 })?.idle).toBe(true);
  });
});
