import { describe, expect, it } from "vitest";

import { helperPromptDecision, updateHold, type HelperPromptInput } from "./helperUpdatePrompt";
import { beginLaunch, launchInFlight, resetLaunchInFlight } from "./launchInFlight";

const older: HelperPromptInput = {
  appVersion: "12.6.31",
  helperVersion: "12.6.30",
  status: "up",
  power: "awake",
  pairing: false,
  stopped: false,
  sending: false,
  autoUpdates: false,
  record: undefined,
};

describe("when the app says the PS5 helper is out of date (#941)", () => {
  it("says it for a helper that answers and is older than this app", () => {
    expect(helperPromptDecision(older)).toEqual({ outdated: true, show: true, notify: true });
  });

  it("says nothing for a helper of this app's version, a newer one, or one it cannot compare", () => {
    for (const helperVersion of ["12.6.31", "12.7.0", null, "dev"]) {
      expect(helperPromptDecision({ ...older, helperVersion })).toEqual({ outdated: false, show: false, notify: false });
    }
    expect(helperPromptDecision({ ...older, appVersion: null }).outdated).toBe(false);
  });

  it("never nags about a console that is asleep, unreachable or not answering", () => {
    expect(helperPromptDecision({ ...older, power: "rest" }).show).toBe(false);
    expect(helperPromptDecision({ ...older, power: "unreachable" }).show).toBe(false);
    expect(helperPromptDecision({ ...older, status: "down" }).show).toBe(false);
    expect(helperPromptDecision({ ...older, status: "unknown" }).show).toBe(false);
    expect(helperPromptDecision({ ...older, status: undefined }).outdated).toBe(false);
  });

  it("stays quiet while this device is not let in yet, and for a helper the user stopped", () => {
    expect(helperPromptDecision({ ...older, pairing: true }).show).toBe(false);
    expect(helperPromptDecision({ ...older, stopped: true }).show).toBe(false);
  });

  it("notifies once per app version: told for this one, the notice stays and nothing new goes out", () => {
    const d = helperPromptDecision({ ...older, record: { notified: "12.6.31" } });
    expect(d).toEqual({ outdated: true, show: true, notify: false });
  });

  it("comes back for the next app version", () => {
    const d = helperPromptDecision({
      ...older,
      appVersion: "12.6.32",
      record: { notified: "12.6.31", dismissed: "12.6.31" },
    });
    expect(d).toEqual({ outdated: true, show: true, notify: true });
  });

  it("a dismissed notice stays away for that app version, and the helper is still marked older", () => {
    const d = helperPromptDecision({ ...older, record: { notified: "12.6.31", dismissed: "12.6.31" } });
    expect(d).toEqual({ outdated: true, show: false, notify: false });
  });

  it("does not notify while a send is replacing the helper", () => {
    expect(helperPromptDecision({ ...older, sending: true })).toEqual({ outdated: true, show: true, notify: false });
  });

  it("leaves a console to the automatic update where that is on", () => {
    expect(helperPromptDecision({ ...older, autoUpdates: true })).toEqual({ outdated: true, show: false, notify: false });
  });
});

describe("why Update helper does not send right now", () => {
  const idle = { transfer: false, task: false, launch: false, stream: false, setup: false };

  it("sends when this app is doing nothing on that console", () => {
    expect(updateHold(idle)).toBeNull();
  });

  it("waits for a transfer, an install, a game start, Remote Play and a bring-up, and says which", () => {
    expect(updateHold({ ...idle, transfer: true })).toBe("transfer");
    expect(updateHold({ ...idle, task: true })).toBe("transfer");
    expect(updateHold({ ...idle, launch: true })).toBe("launch");
    expect(updateHold({ ...idle, stream: true })).toBe("stream");
    expect(updateHold({ ...idle, setup: true })).toBe("busy");
  });
});

describe("game starts in flight", () => {
  it("are counted per console until each one ends, whatever port the address carries", () => {
    resetLaunchInFlight();
    expect(launchInFlight("192.168.1.20")).toBe(false);
    const a = beginLaunch("192.168.1.20:9113");
    const b = beginLaunch("192.168.1.20");
    expect(launchInFlight("192.168.1.20")).toBe(true);
    expect(launchInFlight("192.168.1.21")).toBe(false);
    a();
    a(); // ending twice counts once
    expect(launchInFlight("192.168.1.20")).toBe(true);
    b();
    expect(launchInFlight("192.168.1.20")).toBe(false);
  });
});
