import { describe, expect, it } from "vitest";

import {
  AUTO_UPDATE_AFTER_SEND_MS,
  AUTO_UPDATE_RETRY_MS,
  AUTO_UPDATE_SETTLE_MS,
  helperAge,
  helperAutoUpdateDecision,
  MAX_AUTO_UPDATE_ATTEMPTS,
  type AutoUpdateDecision,
  type AutoUpdateInput,
  type ConsoleCheck,
  type NeverReason,
  type WaitReason,
} from "./helperAutoUpdate";

const idleConsole: ConsoleCheck = {
  reachable: true,
  version: "12.6.13",
  othersBusy: "idle",
  locks: "idle",
  otherRestart: false,
  consoleTasks: "idle",
  saveOpen: "idle",
  cheats: "idle",
  loaderOpen: true,
};

/** An older helper, up a while, nothing running anywhere: the one case that updates. */
const safe: AutoUpdateInput = {
  enabled: true,
  stopped: false,
  appVersion: "12.6.15",
  helperVersion: "12.6.13",
  status: "up",
  power: "awake",
  pairing: false,
  upForMs: 60_000,
  busyHere: null,
  sendInFlight: false,
  sentAgoMs: null,
  attempts: 0,
  lastAttemptAgoMs: null,
  console: idleConsole,
};

const decide = (over: Partial<AutoUpdateInput>, con: Partial<ConsoleCheck> = {}): AutoUpdateDecision =>
  helperAutoUpdateDecision({ ...safe, ...over, console: "console" in over ? over.console! : { ...idleConsole, ...con } });

const wait = (why: WaitReason): AutoUpdateDecision => ({ kind: "wait", why });
const never = (why: NeverReason): AutoUpdateDecision => ({ kind: "never", why });

describe("helperAutoUpdateDecision", () => {
  it("updates an older helper when every safety check passes", () => {
    expect(decide({})).toEqual({ kind: "update" });
  });

  it("asks the console first when this app's own state says yes", () => {
    expect(decide({ console: null })).toEqual({ kind: "check" });
  });

  describe("versions", () => {
    it.each<[string, string | null, string | null, AutoUpdateDecision]>([
      ["equal", "12.6.15", "12.6.15", never("current")],
      ["helper older", "12.6.13", "12.6.15", { kind: "update" }],
      ["helper newer (another device sent it: update this app instead)", "12.6.16", "12.6.15", never("newer")],
      ["helper a pre-release of the app's version", "12.6.15-rc.1", "12.6.15", { kind: "update" }],
      ["app a pre-release, helper the release before", "12.6.14", "12.6.15-rc.1", { kind: "update" }],
      ["app a pre-release of the helper's release (helper newer)", "12.6.15", "12.6.15-rc.1", never("newer")],
      ["helper version unknown", null, "12.6.15", never("unknown-version")],
      ["app version unknown", "12.6.13", null, never("unknown-version")],
      ["unreadable version", "dev", "12.6.15", never("unknown-version")],
    ])("%s", (_name, helper, app, want) => {
      expect(decide({ helperVersion: helper, appVersion: app }, { version: helper })).toEqual(want);
    });

    it("decides again on the version the console gives right before sending", () => {
      // Another device updated it meanwhile.
      expect(decide({}, { version: "12.6.15" })).toEqual(never("current"));
      expect(decide({}, { version: "12.6.16" })).toEqual(never("newer"));
    });

    it("helperAge reads the comparison the same way", () => {
      expect(helperAge("1.0.0", "1.0.1")).toBe("older");
      expect(helperAge("1.0.1", "1.0.1")).toBe("current");
      expect(helperAge("1.0.2", "1.0.1")).toBe("newer");
      expect(helperAge(null, "1.0.1")).toBe("unknown");
    });
  });

  describe("the person's choices", () => {
    it("does nothing with the setting off", () => {
      expect(decide({ enabled: false })).toEqual(never("off"));
    });

    it("leaves a helper the user stopped alone", () => {
      expect(decide({ stopped: true })).toEqual(never("stopped"));
    });
  });

  describe("the console's own state", () => {
    it.each<[string, Partial<AutoUpdateInput>, AutoUpdateDecision]>([
      ["asleep", { power: "rest", status: "down" }, wait("asleep")],
      ["helper not up", { status: "down" }, wait("not-up")],
      ["status not known yet", { status: "unknown" }, wait("not-up")],
      ["no status at all", { status: undefined }, wait("not-up")],
      ["waiting to be paired", { pairing: true }, wait("pairing")],
      ["just came up", { upForMs: AUTO_UPDATE_SETTLE_MS - 1 }, wait("settling")],
      ["up time not known", { upForMs: null }, wait("settling")],
    ])("%s: waits", (_name, over, want) => {
      expect(decide(over)).toEqual(want);
    });

    it("settled right at the limit goes ahead", () => {
      expect(decide({ upForMs: AUTO_UPDATE_SETTLE_MS })).toEqual({ kind: "update" });
    });
  });

  describe("sends and tries", () => {
    it("never sends while another send runs", () => {
      expect(decide({ sendInFlight: true })).toEqual(wait("sending"));
    });

    it("waits out the cooldown after any send to the console", () => {
      expect(decide({ sentAgoMs: AUTO_UPDATE_AFTER_SEND_MS - 1 })).toEqual(wait("recent-send"));
      expect(decide({ sentAgoMs: AUTO_UPDATE_AFTER_SEND_MS })).toEqual({ kind: "update" });
    });

    it("tries again only some minutes after a try that did not work", () => {
      expect(decide({ attempts: 1, lastAttemptAgoMs: AUTO_UPDATE_RETRY_MS - 1 })).toEqual(wait("retry-later"));
      expect(decide({ attempts: 1, lastAttemptAgoMs: AUTO_UPDATE_RETRY_MS })).toEqual({ kind: "update" });
    });

    it("stops trying after a few and leaves it to Send helper", () => {
      expect(decide({ attempts: MAX_AUTO_UPDATE_ATTEMPTS, lastAttemptAgoMs: AUTO_UPDATE_RETRY_MS * 10 })).toEqual(
        never("gave-up"),
      );
    });
  });

  describe("work this app runs there", () => {
    it("waits for a transfer, install, queue, Files, Remote Play or Play Together", () => {
      expect(decide({ busyHere: "Upload Astro Bot" })).toEqual(wait("busy-here"));
    });

    it("does not even ask the console while busy here", () => {
      expect(decide({ busyHere: "Remote Play", console: null })).toEqual(wait("busy-here"));
    });
  });

  describe("what the console says", () => {
    it.each<[string, Partial<ConsoleCheck>, AutoUpdateDecision]>([
      ["a failed or slow probe", { reachable: false }, wait("not-answering")],
      ["other devices busy", { othersBusy: "busy" }, wait("busy-others")],
      ["other devices unknown (Mesh not read)", { othersBusy: "unknown" }, wait("busy-others")],
      ["a lock held (Mesh Drive pass)", { locks: "busy" }, wait("busy-others")],
      ["locks unknown", { locks: "unknown" }, wait("busy-others")],
      ["another device restarting the helper", { otherRestart: true }, wait("another-device")],
      ["a console-page task running (extract, install)", { consoleTasks: "busy" }, wait("console-work")],
      ["a save mounted", { saveOpen: "busy" }, wait("console-work")],
      ["cheats on with a game running", { cheats: "busy" }, wait("console-work")],
      ["the helper itself reports work a restart stops (12.6.15)", { consoleBusy: "busy" }, wait("console-work")],
      ["an older helper that cannot report its work: the other answers decide", { consoleBusy: "unknown" }, { kind: "update" }],
      ["an older helper with no task list", { consoleTasks: "unknown" }, wait("console-unknown")],
      ["a save state it could not tell", { saveOpen: "unknown" }, wait("console-unknown")],
      ["no loader to take the helper", { loaderOpen: false }, wait("loader-closed")],
    ])("%s", (_name, con, want) => {
      expect(decide({}, con)).toEqual(want);
    });

    it("a cheat engine it cannot ask about does not hold the update (nothing is lost)", () => {
      expect(decide({}, { cheats: "unknown" })).toEqual({ kind: "update" });
    });

    it("busy beats a missing loader: the reason given is the one to fix first", () => {
      expect(decide({}, { consoleTasks: "busy", loaderOpen: false })).toEqual(wait("console-work"));
    });
  });
});
