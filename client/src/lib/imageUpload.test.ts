import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Volume } from "../api/ps5";
import { imageDrive, imageDriveChoices, imageDriveNow, imageUploadItem, resolveImagePlan } from "./imageUpload";
import { usePkgStorageStore } from "./pkgStorage";

const fetchVolumes = vi.hoisted(() => vi.fn());
vi.mock("../api/ps5", () => ({ fetchVolumes }));

const drive = (path: string, over: Partial<Volume> = {}): Volume =>
  ({ path, writable: true, is_placeholder: false, free_bytes: 1000, total_bytes: 2000, ...over }) as Volume;
const USB = "/mnt/usb0";
const PRO = "192.0.2.100";
const PHAT = "192.0.2.99";

describe("the drive a game goes to on the console (#673)", () => {
  beforeEach(() => {
    usePkgStorageStore.setState({ defaults: {} });
    fetchVolumes.mockReset();
  });

  it("with nothing picked and nothing set, is internal storage", () => {
    expect(imageDrive(PRO, null, [drive("/data"), drive(USB)])).toEqual({ volume: "/data", fellBack: false });
  });

  it("with nothing picked, is the drive set for packages under Volumes", () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    expect(imageDrive(PRO, null, [drive("/data"), drive(USB)])).toEqual({ volume: USB, fellBack: false });
  });

  it("is decided for each console, not the current one", () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    expect(imageDrive(`${PRO}:9113`, null, [drive(USB)]).volume).toBe(USB);
    expect(imageDrive(PHAT, null, [drive(USB)]).volume).toBe("/data");
  });

  it("falls back as packages do: drive gone, read-only, or the list unreadable", () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    expect(imageDrive(PRO, null, [drive("/data")])).toEqual({ volume: "/data", fellBack: true });
    expect(imageDrive(PRO, null, [drive(USB, { writable: false })])).toEqual({ volume: "/data", fellBack: true });
    expect(imageDrive(PRO, null, null)).toEqual({ volume: "/data", fellBack: true });
  });

  it("uses a drive picked by hand over the one for packages, /data included", () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    const all = [drive("/data"), drive(USB), drive("/mnt/ext0")];
    expect(imageDrive(PRO, "/mnt/ext0", all).volume).toBe("/mnt/ext0");
    expect(imageDrive(PRO, "/data", all)).toEqual({ volume: "/data", fellBack: false });
  });

  it("does not write to a picked drive this console does not have", () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    expect(imageDrive(PRO, "/mnt/ext1", [drive("/data"), drive(USB)])).toEqual({ volume: USB, fellBack: true });
    expect(imageDrive(PHAT, "/mnt/ext1", [drive("/data")])).toEqual({ volume: "/data", fellBack: true });
    // With the list unknown a hand-picked drive stands.
    expect(imageDrive(PRO, "/mnt/ext1", null)).toEqual({ volume: "/mnt/ext1", fellBack: false });
  });

  it("offers only real drives", () => {
    const list = [
      drive("/data"),
      drive(USB),
      drive("/mnt/ro", { writable: false }),
      drive("/mnt/ph", { is_placeholder: true }),
      drive("/mnt/ps5upload/game"),
    ];
    expect(imageDriveChoices(list).map((v) => v.path)).toEqual(["/data", USB]);
    expect(imageDriveChoices(null)).toEqual([]);
  });

  it("asks that console for its drives when a plan is resolved", async () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    fetchVolumes.mockResolvedValue([drive("/data"), drive(USB)]);
    const plan = { host: PRO, volume: null, subpath: "homebrew", deleteAfter: false };
    await expect(resolveImagePlan(plan)).resolves.toEqual({ ...plan, volume: USB });
    expect(fetchVolumes).toHaveBeenCalledWith(PRO);
    const item = imageUploadItem("/out/Game.ffpfsc", await resolveImagePlan(plan));
    expect(item.resolvedDest).toBe(`${USB}/homebrew/Game.ffpfsc`);
  });

  it("falls back when the console cannot be asked, and need not ask for internal storage", async () => {
    usePkgStorageStore.getState().setDefault(PRO, USB);
    fetchVolumes.mockRejectedValue(new Error("no route"));
    await expect(imageDriveNow(PRO, null)).resolves.toEqual({ volume: "/data", fellBack: true });
    fetchVolumes.mockClear();
    await expect(imageDriveNow(PRO, "/data")).resolves.toEqual({ volume: "/data", fellBack: false });
    expect(fetchVolumes).not.toHaveBeenCalled();
  });
});

describe("a built image as an Upload item", () => {
  it("goes into the chosen folder on the PS5, named after the image", () => {
    const it1 = imageUploadItem(
      "/Users/me/out/Worms.ffpfsc",
      {
        host: "192.168.1.5",
        volume: "/mnt/ext0",
        subpath: "homebrew",
        deleteAfter: true,
      },
      2_400_000_000,
    );
    expect(it1.sourceKind).toBe("image");
    expect(it1.resolvedDest).toBe("/mnt/ext0/homebrew/Worms.ffpfsc");
    expect(it1.addr).toBe("192.168.1.5");
    expect(it1.deleteSourceAfterUpload).toBe(true);
    expect(it1.estimatedBytes).toBe(2_400_000_000);
    expect(it1.mountAfterUpload).toBe(false);
  });

  it("defaults to /data when no drive was picked, and keeps the image unless asked", () => {
    const it1 = imageUploadItem("C:\\out\\Game.exfat", {
      host: "10.0.0.2:9120",
      volume: null,
      subpath: "/homebrew/",
      deleteAfter: false,
    });
    expect(it1.resolvedDest).toBe("/data/homebrew/Game.exfat");
    expect(it1.deleteSourceAfterUpload).toBe(false);
  });
});
