import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { canEjectUsb, usbEjectFlow, type UsbEjectDeps } from "./useUsbEject";
import type { Translator } from "../state/lang";
import type { ConfirmOptions } from "../components/ConfirmDialog";

const tr = ((_k: string, a?: unknown, b?: string) => {
  const s = typeof a === "string" ? a : (b ?? _k);
  const vars = (typeof a === "object" && a) || {};
  return s.replace(/\{(\w+)\}/g, (_, k: string) => String((vars as Record<string, unknown>)[k] ?? ""));
}) as Translator;

function deps(over: Partial<UsbEjectDeps> = {}) {
  const base = {
    confirm: vi.fn(async (_o: ConfirmOptions) => true),
    eject: vi.fn(async (_h: string, _m: string, _f: boolean): Promise<unknown> => undefined),
    isStale: vi.fn(() => false),
    onBusy: vi.fn((_b: boolean) => {}),
  };
  // Overrides are mocks too; keep the mock types for the assertions.
  return Object.assign(base, over) as typeof base;
}

describe("USB eject", () => {
  it("offers eject for /mnt/usb0-7 only", () => {
    expect(canEjectUsb("/mnt/usb0")).toBe(true);
    expect(canEjectUsb("/mnt/usb7")).toBe(true);
    expect(canEjectUsb("/mnt/usb8")).toBe(false);
    expect(canEjectUsb("/mnt/ext0")).toBe(false);
    expect(canEjectUsb("/data")).toBe(false);
    expect(canEjectUsb("/mnt/usb0/games")).toBe(false);
  });

  it("asks, then ejects without force", async () => {
    const d = deps();
    expect(await usbEjectFlow("h", "/mnt/usb0", d, tr)).toEqual({ kind: "ejected" });
    expect(d.confirm).toHaveBeenCalledTimes(1);
    expect(d.eject).toHaveBeenCalledWith("h", "/mnt/usb0", false);
    expect(d.onBusy.mock.calls).toEqual([[true], [false]]);
  });

  it("says why when the engine refuses: its own job on the drive, or a game for a force", async () => {
    const jobs = deps({ eject: vi.fn(async () => Promise.reject(new Error("usb_eject_refused_jobs"))) });
    const out = await usbEjectFlow("h", "/mnt/usb0", jobs, tr);
    expect(out).toMatchObject({ kind: "failed", message: expect.stringContaining("upload") });
    // Refused outright: never offered as "eject anyway".
    expect(jobs.confirm).toHaveBeenCalledTimes(1);
    const eject = vi
      .fn(async (_h: string, _m: string, force: boolean): Promise<unknown> => {
        throw new Error(force ? "usb_eject_refused_game" : "usb_eject_busy");
      });
    const out2 = await usbEjectFlow("h", "/mnt/usb0", deps({ eject }), tr);
    expect(out2).toMatchObject({ kind: "failed", message: expect.stringContaining("game") });
  });

  it("does nothing when the user cancels", async () => {
    const d = deps({ confirm: vi.fn(async () => false) });
    expect(await usbEjectFlow("h", "/mnt/usb0", d, tr)).toEqual({ kind: "cancelled" });
    expect(d.eject).not.toHaveBeenCalled();
  });

  it("does nothing when the console changed while the dialog was open", async () => {
    const d = deps({ isStale: vi.fn(() => true) });
    expect(await usbEjectFlow("h", "/mnt/usb0", d, tr)).toEqual({ kind: "cancelled" });
    expect(d.eject).not.toHaveBeenCalled();
  });

  it("offers to force when the drive is busy, and forces on yes", async () => {
    const eject = vi
      .fn()
      .mockRejectedValueOnce(new Error("usb_eject_busy"))
      .mockResolvedValueOnce(undefined);
    const d = deps({ eject });
    expect(await usbEjectFlow("h", "/mnt/usb1", d, tr)).toEqual({ kind: "ejected" });
    expect(d.confirm).toHaveBeenCalledTimes(2);
    expect(d.confirm.mock.calls[1][0].destructive).toBe(true);
    expect(eject.mock.calls).toEqual([
      ["h", "/mnt/usb1", false],
      ["h", "/mnt/usb1", true],
    ]);
  });

  it("leaves a busy drive mounted when the user declines to force", async () => {
    const eject = vi.fn().mockRejectedValueOnce(new Error("usb_eject_busy"));
    const confirm = vi.fn().mockResolvedValueOnce(true).mockResolvedValueOnce(false);
    const d = deps({ eject, confirm });
    expect(await usbEjectFlow("h", "/mnt/usb0", d, tr)).toEqual({ kind: "cancelled" });
    expect(eject).toHaveBeenCalledTimes(1);
  });

  it("reports any other failure with the drive's path", async () => {
    const d = deps({ eject: vi.fn().mockRejectedValue(new Error("not_mounted")) });
    const out = await usbEjectFlow("h", "/mnt/usb0", d, tr);
    expect(out).toEqual({ kind: "failed", message: "Couldn't eject /mnt/usb0: not_mounted" });
    expect(d.confirm).toHaveBeenCalledTimes(1);
    expect(d.onBusy).toHaveBeenLastCalledWith(false);
  });
});
