import { describe, expect, it, vi } from "vitest";
import {
  autoRunStillLoaded,
  autoStepDecision,
  bootEpochMs,
  distinctPayloads,
  kernelPatcherOf,
  loadKernelSends,
  rememberKernelSend,
  stepRunning,
  type AutoRunContext,
} from "./autoLoaderGuard";

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

describe("an automatic run decides each step", () => {
  const kstuff = { path: "/payloads/kstuff-lite-1.11.elf" };
  const smp = { path: "console:/data/payloads/shadowmountplus-1.7.elf" };
  const other = { path: "remote://nas-1a2b/elf/ftpsrv.elf" };
  const bootMs = 90 * HOUR;
  const ctx = (c: Partial<AutoRunContext>): AutoRunContext => ({
    running: null,
    bootMs: null,
    stayedSince: () => null,
    ...c,
  });

  it("knows kernel patchers by file name or catalogue id", () => {
    expect(kernelPatcherOf(kstuff)).toBe("kstuff");
    expect(kernelPatcherOf({ path: "", payloadId: "kstuff-echostretch" })).toBe("kstuff");
    expect(kernelPatcherOf({ path: "C:\\elf\\etaHEN-2.0.bin" })).toBe("etahen");
    expect(kernelPatcherOf(smp)).toBeNull();
  });

  it("never sends what the process list shows running", () => {
    const running = ["SceSysCore.elf", "kstuff.elf", "shadowmountplus.elf", "payload.elf"];
    expect(autoStepDecision(kstuff, ctx({ running }), {})).toEqual({ send: false, why: "running" });
    expect(autoStepDecision(smp, ctx({ running }), {})).toEqual({ send: false, why: "running" });
    expect(autoStepDecision(other, ctx({ running }), {})).toEqual({ send: true });
  });

  it("does not take a generic loader name for any payload", () => {
    expect(stepRunning({ path: "/x/payload.elf" }, ["payload.elf", "elfldr.elf"])).toBe(false);
    expect(stepRunning(kstuff, ["payload.elf", "elfldr.elf", "ps5upload.elf"])).toBe(false);
  });

  // The kernel-panic report: kstuff shows only as `payload.elf`, and every time the helper came
  // back the auto-run sent it again.
  it("never sends kstuff twice in one console boot when it shows as payload.elf", () => {
    const running = ["SceSysCore.elf", "payload.elf", "elfldr.elf", "ps5upload.elf"];
    const sentThisBoot = { kstuff: bootMs + 60_000 };
    expect(autoStepDecision(kstuff, ctx({ running, bootMs }), sentThisBoot)).toEqual({
      send: false,
      why: "sent-this-boot",
    });
    // Other steps still go.
    expect(autoStepDecision(other, ctx({ running, bootMs }), sentThisBoot)).toEqual({ send: true });
  });

  it("sends kstuff again after a reboot", () => {
    const sentLastBoot = { kstuff: bootMs - HOUR };
    expect(autoStepDecision(kstuff, ctx({ running: [], bootMs }), sentLastBoot)).toEqual({ send: true });
    // Never sent from here: the playlist sends it, as before.
    expect(autoStepDecision(kstuff, ctx({ running: [], bootMs }), {})).toEqual({ send: true });
  });

  it("with no helper to ask, holds back only when the console stayed up since the send", () => {
    const sent = { kstuff: 1_000 };
    // The helper crashed: the console never went away since kstuff was sent.
    expect(autoStepDecision(kstuff, ctx({ stayedSince: () => true }), sent)).toEqual({
      send: false,
      why: "sent-this-boot",
    });
    // It went away since (off, a restart): a new boot needs kstuff.
    expect(autoStepDecision(kstuff, ctx({ stayedSince: () => false }), sent)).toEqual({ send: true });
    // Not watched since then (the app started later) and no uptime: sent, as before.
    expect(autoStepDecision(kstuff, ctx({ stayedSince: () => null }), sent)).toEqual({ send: true });
  });

  it("remembers kernel sends per console", () => {
    const store = new Map<string, string>();
    vi.stubGlobal("window", {
      localStorage: {
        getItem: (k: string) => store.get(k) ?? null,
        setItem: (k: string, v: string) => void store.set(k, v),
      },
    });
    rememberKernelSend("10.0.0.9", "kstuff", 5);
    rememberKernelSend("10.0.0.9", "etahen", 7);
    expect(loadKernelSends("10.0.0.9")).toEqual({ kstuff: 5, etahen: 7 });
    expect(loadKernelSends("10.0.0.10")).toEqual({});
    vi.unstubAllGlobals();
  });
});
