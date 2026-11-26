import { describe, expect, it, vi } from "vitest";

import { backUpTarget, filesBackStep, folderTitle, revealAboveKeyboard } from "./phoneFiles";

const at = (path: string, extra: Partial<Parameters<typeof filesBackStep>[0]> = {}) =>
  filesBackStep({ view: false, selecting: false, path, driveRoot: "/mnt/usb0", ...extra });

describe("system Back on the Files listing (#932)", () => {
  it("leaves selection mode first, wherever the folder is", () => {
    expect(at("/mnt/usb0/homebrew", { selecting: true })).toBe("end-selection");
    expect(at("/mnt/usb0", { selecting: true })).toBe("end-selection");
  });

  it("then lets go of what is staged to copy or move, at a drive's root too (#936)", () => {
    expect(at("/mnt/usb0/homebrew", { selecting: true, staged: true })).toBe("end-selection");
    // Staged items stay while Back goes up to the folder to paste into; at the top Back lets go.
    expect(at("/mnt/usb0/homebrew", { staged: true })).toBe("up");
    expect(at("/mnt/usb0", { staged: true })).toBe("cancel-staged");
    expect(at("/mnt/usb0/homebrew", { staged: true, view: true })).toBeNull();
  });

  it("then goes up one folder", () => {
    expect(at("/mnt/usb0/homebrew/games")).toBe("up");
    expect(backUpTarget("/mnt/usb0/homebrew/games", "/mnt/usb0")).toBe("/mnt/usb0/homebrew");
  });

  it("leaves Files from the drive's root", () => {
    expect(at("/mnt/usb0")).toBeNull();
    expect(at("/mnt/usb0/")).toBeNull();
  });

  it("goes up to / outside every known drive, and leaves from there", () => {
    expect(at("/system_ex/app", { driveRoot: null })).toBe("up");
    expect(backUpTarget("/system_ex", null)).toBe("/");
    expect(at("/", { driveRoot: null })).toBeNull();
  });

  it("takes no step while a view covers the listing (it is its own history entry)", () => {
    expect(at("/mnt/usb0/homebrew", { view: true, selecting: true })).toBeNull();
  });
});

describe("the compact header's title (#936)", () => {
  it("is the open folder's own name", () => {
    expect(folderTitle("/mnt/usb0/homebrew")).toBe("homebrew");
    expect(folderTitle("/data/")).toBe("data");
  });

  it("is left to the screen's name at /", () => {
    expect(folderTitle("/")).toBeNull();
  });
});

describe("a name field opened on a phone (#936)", () => {
  it("is brought into view on a phone, and again when the keyboard shrinks the screen", () => {
    const el = { scrollIntoView: vi.fn() } as unknown as HTMLElement;
    const listeners: Array<() => void> = [];
    vi.stubGlobal("window", {
      visualViewport: {
        addEventListener: (_: string, f: () => void) => listeners.push(f),
        removeEventListener: vi.fn(),
      },
      setTimeout: vi.fn(),
    });
    try {
      revealAboveKeyboard(el, true);
      expect(el.scrollIntoView).toHaveBeenCalledTimes(1);
      listeners.forEach((f) => f());
      expect(el.scrollIntoView).toHaveBeenCalledTimes(2);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("is left alone on a computer", () => {
    const el = { scrollIntoView: vi.fn() } as unknown as HTMLElement;
    revealAboveKeyboard(el, false);
    expect(el.scrollIntoView).not.toHaveBeenCalled();
  });
});
