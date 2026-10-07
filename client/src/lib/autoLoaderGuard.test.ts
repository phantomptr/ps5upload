import { describe, expect, it } from "vitest";
import { autoRunStillLoaded, bootEpochMs, distinctPayloads } from "./autoLoaderGuard";

const HOUR = 3_600_000;
const boot = bootEpochMs(100 * HOUR, 10 * 3600); // booted at hour 90
const record = { bootEpochMs: boot, payloads: ["kstuff.elf", "shadowmountplus.elf"] };
const running = ["SceSysCore.elf", "kstuff.elf", "shadowmountplus.elf", "payload.elf", "elfldr.elf"];

describe("the auto-loader's same-boot guard", () => {
  it("skips the run when it is the same boot and its payloads are still running", () => {
    // A Wi-Fi blip an hour later: same boot, kstuff and ShadowMount+ still there.
    expect(autoRunStillLoaded(record, bootEpochMs(101 * HOUR, 11 * 3600), running)).toBe(true);
  });

  it("runs again after a reboot", () => {
    expect(autoRunStillLoaded(record, bootEpochMs(101 * HOUR, 60), running)).toBe(false);
  });

  it("runs again after rest mode, which kills the payloads without changing the boot time", () => {
    const afterWake = ["SceSysCore.elf", "payload.elf", "elfldr.elf"];
    expect(autoRunStillLoaded(record, boot, afterWake)).toBe(false);
    // One of the two gone is enough.
    expect(autoRunStillLoaded(record, boot, [...afterWake, "kstuff.elf"])).toBe(false);
  });

  it("never skips without evidence", () => {
    expect(autoRunStillLoaded(null, boot, running)).toBe(false);
    expect(autoRunStillLoaded(record, null, running)).toBe(false);
    expect(autoRunStillLoaded(record, boot, null)).toBe(false);
    // Nothing distinct was left by the last run: nothing to recognise it by.
    expect(autoRunStillLoaded({ bootEpochMs: boot, payloads: [] }, boot, running)).toBe(false);
  });

  it("remembers only payloads that can be recognised later", () => {
    expect(
      distinctPayloads([
        "SceShellUI",
        "SceSysCore.elf",
        "AgcCompositor.elf",
        "orbis_audiod.elf",
        "mini-syscore.elf",
        "payload.elf",
        "elfldr.elf",
        "ps5upload.elf",
        "kstuff.elf",
        "shadowmountplus.elf",
        "kstuff.elf",
        "ftpsrv.elf",
      ]),
    ).toEqual(["ftpsrv.elf", "kstuff.elf", "shadowmountplus.elf"]);
  });
});
