import { beforeEach, describe, expect, it, vi } from "vitest";

const { mockIsIOS, mockIsAndroid, mockRoots, mockList, mockSave, mockDownloads } = vi.hoisted(() => ({
  mockIsIOS: vi.fn(),
  mockIsAndroid: vi.fn(),
  mockRoots: vi.fn(),
  mockList: vi.fn(),
  mockSave: vi.fn(),
  mockDownloads: vi.fn(),
}));

vi.mock("./platform", () => ({ isIOS: () => mockIsIOS(), isAndroid: () => mockIsAndroid() }));
vi.mock("../api/localFs", () => ({
  localFs: {
    storageRoots: () => mockRoots(),
    listDir: (p: string) => mockList(p),
    downloadsDir: () => mockDownloads(),
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: (o: unknown) => mockSave(o) }));

import { pickSaveDest } from "./pickSaveDest";

const DOCS = "/var/mobile/Containers/Data/Application/X/Documents";

describe("pickSaveDest", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockRoots.mockResolvedValue([DOCS]);
    mockDownloads.mockResolvedValue("/storage/emulated/0/Download");
    mockList.mockResolvedValue([]);
    mockIsAndroid.mockReturnValue(false);
  });

  it("uses the system save dialog off iPhone", async () => {
    mockIsIOS.mockReturnValue(false);
    mockSave.mockResolvedValue("/Users/me/x.zip");
    expect(await pickSaveDest({ defaultPath: "x.zip" })).toBe("/Users/me/x.zip");
    expect(mockSave).toHaveBeenCalledWith({ defaultPath: "x.zip" });
  });

  it("keeps a cancelled dialog as null", async () => {
    mockIsIOS.mockReturnValue(false);
    mockSave.mockResolvedValue(null);
    expect(await pickSaveDest({ defaultPath: "x.zip" })).toBeNull();
  });

  it("writes into the app's Documents folder on iPhone, with no dialog", async () => {
    mockIsIOS.mockReturnValue(true);
    expect(await pickSaveDest({ defaultPath: "ps5upload-logs.txt" })).toBe(`${DOCS}/ps5upload-logs.txt`);
    expect(mockSave).not.toHaveBeenCalled();
  });

  it("never replaces a file already there", async () => {
    mockIsIOS.mockReturnValue(true);
    mockList.mockResolvedValue([{ name: "save.zip" }, { name: "save (2).zip" }]);
    expect(await pickSaveDest({ defaultPath: "some/dir/save.zip" })).toBe(`${DOCS}/save (3).zip`);
  });

  it("names an export with no suggestion", async () => {
    mockIsIOS.mockReturnValue(true);
    expect(await pickSaveDest({})).toBe(`${DOCS}/ps5upload-export`);
  });

  it("writes straight into Downloads on Android: the dialog left an empty file behind", async () => {
    mockIsIOS.mockReturnValue(false);
    mockIsAndroid.mockReturnValue(true);
    mockList.mockResolvedValue([{ name: "report.zip" }]);
    expect(await pickSaveDest({ defaultPath: "report.zip" })).toBe("/storage/emulated/0/Download/report (2).zip");
    expect(mockList).toHaveBeenCalledWith("/storage/emulated/0/Download");
    expect(mockSave).not.toHaveBeenCalled();
  });

  it("uses this user's Downloads, and the primary user's when Android cannot say", async () => {
    mockIsIOS.mockReturnValue(false);
    mockIsAndroid.mockReturnValue(true);
    mockList.mockRejectedValue(new Error("no such folder"));
    mockDownloads.mockResolvedValueOnce("/storage/emulated/10/Download");
    expect(await pickSaveDest({ defaultPath: "x.csv" })).toBe("/storage/emulated/10/Download/x.csv");
    mockDownloads.mockRejectedValueOnce(new Error("no bridge"));
    expect(await pickSaveDest({ defaultPath: "x.csv" })).toBe("/storage/emulated/0/Download/x.csv");
  });

  it("an unreadable Downloads folder still gets the name as it is", async () => {
    mockIsIOS.mockReturnValue(false);
    mockIsAndroid.mockReturnValue(true);
    mockList.mockRejectedValue(new Error("no access"));
    expect(await pickSaveDest({ defaultPath: "x.csv" })).toBe("/storage/emulated/0/Download/x.csv");
  });
});
