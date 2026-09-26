import { beforeEach, describe, expect, it, vi } from "vitest";

// Mock the OS-notify helper so we can assert the mirror behavior without
// the Tauri plugin. `state.foreground` is controllable per test; hoisted
// so the vi.mock factory can reference it.
const { mockSend, state } = vi.hoisted(() => ({
  mockSend: vi.fn(),
  state: { foreground: true },
}));

vi.mock("../lib/osNotify", () => ({
  sendOsNotification: (...args: unknown[]) => mockSend(...args),
  appIsForeground: () => state.foreground,
  ensureOsNotificationPermission: vi.fn(async () => false),
}));

import { useNotificationsStore } from "./notifications";

describe("notifications → OS notification mirror", () => {
  beforeEach(() => {
    mockSend.mockClear();
    state.foreground = true;
    useNotificationsStore.setState({ entries: [], osNotifyEnabled: true });
  });

  it("mirrors to the OS when the app is backgrounded and the toggle is on", () => {
    state.foreground = false;
    useNotificationsStore
      .getState()
      .push("error", "Upload failed", { body: "connection refused" });
    expect(mockSend).toHaveBeenCalledTimes(1);
    expect(mockSend).toHaveBeenCalledWith(
      "error",
      "Upload failed",
      "connection refused",
    );
  });

  it("does NOT mirror while the app is in the foreground (no double-notify)", () => {
    state.foreground = true;
    useNotificationsStore.getState().push("info", "Heads up");
    expect(mockSend).not.toHaveBeenCalled();
  });

  it("does NOT mirror when the OS toggle is disabled, even if backgrounded", () => {
    state.foreground = false;
    useNotificationsStore.setState({ osNotifyEnabled: false });
    useNotificationsStore.getState().push("error", "should not surface");
    expect(mockSend).not.toHaveBeenCalled();
  });

  it("always records the entry in the in-app inbox regardless of mirroring", () => {
    state.foreground = true; // not mirrored, but still inboxed
    const id = useNotificationsStore.getState().push("success", "Done");
    const entries = useNotificationsStore.getState().entries;
    expect(entries[0]?.id).toBe(id);
    expect(entries[0]?.title).toBe("Done");
    expect(entries[0]?.level).toBe("success");
  });
});
