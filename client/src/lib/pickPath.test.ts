import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the three boundaries pickPath dispatches across: platform
// detection, the in-app picker (Android), and the native dialog (desktop).
const { mockIsAndroid, mockPickLocal, mockOpenDialog } = vi.hoisted(() => ({
  mockIsAndroid: vi.fn(),
  mockPickLocal: vi.fn(),
  mockOpenDialog: vi.fn(),
}));

vi.mock("./platform", () => ({ isAndroid: () => mockIsAndroid() }));
vi.mock("../state/localPicker", () => ({
  pickLocalPath: (...a: unknown[]) => mockPickLocal(...a),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: (...a: unknown[]) => mockOpenDialog(...a),
}));

import { pickPath } from "./pickPath";

beforeEach(() => {
  mockIsAndroid.mockReset();
  mockPickLocal.mockReset();
  mockOpenDialog.mockReset();
});

describe("pickPath on Android", () => {
  beforeEach(() => mockIsAndroid.mockReturnValue(true));

  it("routes a folder pick to the in-app browser, not the native dialog", async () => {
    mockPickLocal.mockResolvedValue("/storage/emulated/0/Download/Game");
    const p = await pickPath({ mode: "folder", title: "Pick" });
    expect(p).toBe("/storage/emulated/0/Download/Game");
    expect(mockPickLocal).toHaveBeenCalledWith({ mode: "folder", title: "Pick" });
    expect(mockOpenDialog).not.toHaveBeenCalled();
  });

  it("routes a file pick to the in-app browser", async () => {
    mockPickLocal.mockResolvedValue("/storage/emulated/0/x.pkg");
    const p = await pickPath({ mode: "file" });
    expect(p).toBe("/storage/emulated/0/x.pkg");
    expect(mockPickLocal).toHaveBeenCalledWith({ mode: "file", title: undefined });
    expect(mockOpenDialog).not.toHaveBeenCalled();
  });

  it("propagates a cancel (null)", async () => {
    mockPickLocal.mockResolvedValue(null);
    expect(await pickPath({ mode: "folder" })).toBeNull();
  });
});

describe("pickPath on desktop", () => {
  beforeEach(() => mockIsAndroid.mockReturnValue(false));

  it("uses the native dialog with directory:true for folders", async () => {
    mockOpenDialog.mockResolvedValue("/Users/me/games");
    const p = await pickPath({ mode: "folder", title: "Dest" });
    expect(p).toBe("/Users/me/games");
    expect(mockOpenDialog).toHaveBeenCalledWith({
      directory: true,
      multiple: false,
      title: "Dest",
      filters: undefined,
    });
    expect(mockPickLocal).not.toHaveBeenCalled();
  });

  it("uses directory:false + filters for files", async () => {
    mockOpenDialog.mockResolvedValue("/Users/me/p.pkg");
    const filters = [{ name: "PS5 Package", extensions: ["pkg"] }];
    await pickPath({ mode: "file", filters });
    expect(mockOpenDialog).toHaveBeenCalledWith({
      directory: false,
      multiple: false,
      title: undefined,
      filters,
    });
  });

  it("returns null when the native dialog is cancelled (non-string)", async () => {
    mockOpenDialog.mockResolvedValue(null);
    expect(await pickPath({ mode: "file" })).toBeNull();
  });
});
