import { beforeEach, describe, expect, it, vi } from "vitest";

// Save to Photos in the phone apps (9.11): the native command adds it to the photo library; a
// refusal falls back to a file in Downloads, and says Photos was refused.

const env = vi.hoisted(() => ({ tauri: true, mobile: true }));
const invoke = vi.hoisted(() => vi.fn());

vi.mock("./tauriEnv", () => ({ isTauriEnv: () => env.tauri }));
vi.mock("./platform", () => ({ isMobile: () => env.mobile }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("./pickSaveDest", () => ({ downloadsDest: async (name: string) => `/Downloads/${name}` }));
vi.mock("../state/logs", () => ({ log: { warn: () => {} } }));

import { appSavesToPhotos, captureFile, saveCapture } from "./rpCapture";

const png = new Blob([new Uint8Array([137, 80, 78, 71])], { type: "image/png" });

describe("saveCapture to Photos", () => {
  beforeEach(() => {
    env.tauri = true;
    env.mobile = true;
    invoke.mockReset();
  });

  it("hands the phone app the bytes, the name and the type", async () => {
    invoke.mockResolvedValueOnce("Photos");
    await expect(saveCapture(png, "PS5 Pro 2026-10-30 21-14-05.png", true)).resolves.toEqual({
      where: "Photos",
      photos: true,
    });
    expect(invoke).toHaveBeenCalledTimes(1);
    const [cmd, body, opts] = invoke.mock.calls[0] as [string, Uint8Array, { headers: Record<string, string> }];
    expect(cmd).toBe("save_to_photos");
    expect([...body]).toEqual([137, 80, 78, 71]);
    expect(decodeURIComponent(opts.headers["x-name"])).toBe("PS5 Pro 2026-10-30 21-14-05.png");
    expect(opts.headers["x-mime"]).toBe("image%2Fpng");
  });

  it("falls back to Downloads when Photos is refused, and says so", async () => {
    invoke.mockRejectedValueOnce("photos_denied").mockResolvedValueOnce("/Downloads/a.png");
    await expect(saveCapture(png, "a.png", true)).resolves.toEqual({ where: "/Downloads/a.png", photos: false, denied: true });
    expect(invoke.mock.calls[1][0]).toBe("save_binary_file");
  });

  it("falls back quietly on any other failure (old Android)", async () => {
    invoke.mockRejectedValueOnce("unsupported").mockResolvedValueOnce("/Downloads/a.png");
    await expect(saveCapture(png, "a.png", true)).resolves.toEqual({ where: "/Downloads/a.png", photos: false });
  });

  it("goes to Downloads when the setting is off, and on a computer", async () => {
    invoke.mockResolvedValue("/Downloads/a.png");
    await saveCapture(png, "a.png", false);
    env.mobile = false;
    expect(appSavesToPhotos()).toBe(false);
    await saveCapture(png, "a.png", true);
    expect(invoke.mock.calls.map((c) => c[0])).toEqual(["save_binary_file", "save_binary_file"]);
  });
});

describe("captureFile", () => {
  it("keeps the blob's type, or names one from the extension", () => {
    expect(captureFile(png, "a.png").type).toBe("image/png");
    expect(captureFile(new Blob([]), "a.webm").type).toBe("video/webm");
    expect(captureFile(new Blob([]), "a.mp4").type).toBe("video/mp4");
    expect(captureFile(new Blob([]), "a.png").name).toBe("a.png");
  });
});
