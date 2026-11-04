import { describe, expect, it } from "vitest";

import { inboxPathsNeeded } from "./iosInbox";

const inbox = "/private/var/mobile/Containers/Data/Application/X/tmp/app-Inbox/Game.zip";

describe("the iPhone's picked copies the queue still needs", () => {
  it("keeps waiting, running and failed items (a failed one can be retried)", () => {
    for (const status of ["pending", "running", "failed"] as const) {
      expect(inboxPathsNeeded([{ sourcePath: inbox, status }]).has(inbox), status).toBe(true);
    }
  });

  it("lets a finished item's copy go", () => {
    expect(inboxPathsNeeded([{ sourcePath: inbox, status: "done" }]).size).toBe(0);
  });

  it("keeps a copy another item still needs", () => {
    const items = [
      { sourcePath: inbox, status: "done" as const },
      { sourcePath: inbox, status: "pending" as const },
    ];
    expect(inboxPathsNeeded(items).has(inbox)).toBe(true);
  });

  it("ignores everything outside an Inbox folder", () => {
    expect(inboxPathsNeeded([{ sourcePath: "/var/mobile/Documents/Game.zip", status: "pending" }]).size).toBe(0);
  });
});
