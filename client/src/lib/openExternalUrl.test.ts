import { afterEach, describe, expect, it, vi } from "vitest";

// Mock the opener plugin so the helper can be exercised without Tauri.
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
// Mock the logs store so the failure path's log.warn doesn't touch real state.
vi.mock("../state/logs", () => ({ log: { warn: vi.fn(), info: vi.fn() } }));

import { openUrl } from "@tauri-apps/plugin-opener";
import { log } from "../state/logs";
import { openExternalUrl } from "./openExternalUrl";

const mockedOpen = vi.mocked(openUrl);
const mockedWarn = vi.mocked(log.warn);

afterEach(() => {
  mockedOpen.mockReset();
  mockedWarn.mockReset();
});

describe("openExternalUrl", () => {
  it("opens the URL via the opener plugin and returns true", async () => {
    mockedOpen.mockResolvedValue(undefined);
    const ok = await openExternalUrl("https://github.com/phantomptr/ps5upload");
    expect(ok).toBe(true);
    expect(mockedOpen).toHaveBeenCalledWith(
      "https://github.com/phantomptr/ps5upload",
    );
    expect(mockedWarn).not.toHaveBeenCalled();
  });

  it("returns false and LOGS (does not throw) when the opener fails", async () => {
    // The exact failure that broke Android: the old shell open's IO error.
    mockedOpen.mockRejectedValue(
      new Error("Scoped shell IO error: No such file or directory (os error 2)"),
    );
    // Must not throw — fire-and-forget callers depend on that.
    const ok = await openExternalUrl("https://example.com/app.apk");
    expect(ok).toBe(false);
    // Logged so a "links don't open" report has a trace.
    expect(mockedWarn).toHaveBeenCalledTimes(1);
    const [category, message] = mockedWarn.mock.calls[0];
    expect(category).toBe("ui");
    expect(message).toContain("https://example.com/app.apk");
  });
});
