import { beforeEach, describe, expect, it, vi } from "vitest";

const recordAppEvent = vi.fn(() => 1234);
vi.mock("../lib/appJournal", () => ({ recordAppEvent: (...a: unknown[]) => recordAppEvent(...(a as [])) }));

import { pushNotification, useNotificationsStore } from "./notifications";

// An error the user is told about is in the bug-report journal, and "Jump to" opens the report
// with that event pinned.
describe("notifications feed the app journal", () => {
  beforeEach(() => {
    recordAppEvent.mockClear();
    useNotificationsStore.setState({ entries: [] });
  });

  it("records an error and links it to the bug report", () => {
    pushNotification("error", "Upload failed", { body: "connection lost" });
    expect(recordAppEvent).toHaveBeenCalledWith(
      expect.objectContaining({ cat: "app", level: "error", code: "notification", msg: "Upload failed: connection lost" }),
    );
    const n = useNotificationsStore.getState().entries[0];
    expect(n.link).toBe("/bug-report?event=1234");
  });

  it("keeps a link the caller set", () => {
    pushNotification("error", "Install failed", { link: "/install" });
    expect((useNotificationsStore.getState().entries[0]).link).toBe("/install");
  });

  it("does not journal plain info", () => {
    pushNotification("info", "Copied");
    expect(recordAppEvent).not.toHaveBeenCalled();
  });
});
