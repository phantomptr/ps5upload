import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { installStorage } from "../test-utils/storage";

vi.mock("../api/ps5", () => ({
  updateCheck: vi.fn(async () => ({
    available: false,
    current_version: "1.0.0",
    latest_version: "1.0.0",
    notes: "",
    pub_date: "",
    download_url: "",
    download_filename: "",
  })),
  updateDownload: vi.fn(),
}));

const CHANNEL_KEY = "ps5upload.update.channel";

describe("update channel preference", () => {
  beforeEach(() => {
    installStorage();
    vi.resetModules();
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("defaults to stable when nothing is stored", async () => {
    // The pre-release channel must be opt-in: defaulting the other way would
    // offer builds nobody has run on hardware to every user.
    const { useUpdateStore } = await import("./update");
    expect(useUpdateStore.getState().channel).toBe("stable");
  });

  it("treats an unrecognised stored value as stable", async () => {
    // A corrupt value, or one written by a future build, must fail safe.
    localStorage.setItem(CHANNEL_KEY, "banana");
    const { useUpdateStore } = await import("./update");
    expect(useUpdateStore.getState().channel).toBe("stable");
  });

  it("reads a stored opt-in back", async () => {
    localStorage.setItem(CHANNEL_KEY, "prerelease");
    const { useUpdateStore } = await import("./update");
    expect(useUpdateStore.getState().channel).toBe("prerelease");
  });

  it("persists a change so it survives a restart", async () => {
    const { useUpdateStore } = await import("./update");
    useUpdateStore.getState().setChannel("prerelease");
    expect(localStorage.getItem(CHANNEL_KEY)).toBe("prerelease");
    useUpdateStore.getState().setChannel("stable");
    expect(localStorage.getItem(CHANNEL_KEY)).toBe("stable");
  });

  it("asks the updater for the channel the user chose", async () => {
    const api = await import("../api/ps5");
    const { useUpdateStore } = await import("./update");
    useUpdateStore.getState().setChannel("prerelease");
    await useUpdateStore.getState().checkNow();
    expect(api.updateCheck).toHaveBeenCalledWith("prerelease");
  });
});
