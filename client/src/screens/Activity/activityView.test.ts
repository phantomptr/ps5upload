import { describe, expect, it } from "vitest";

import { parseActivityTab, stopActionFor } from "./activityView";

describe("parseActivityTab", () => {
  it("opens the named tab, tasks by default", () => {
    expect(parseActivityTab(null)).toBe("tasks");
    expect(parseActivityTab("history")).toBe("history");
    expect(parseActivityTab("stats")).toBe("stats");
    expect(parseActivityTab("bogus")).toBe("tasks");
  });

  it("sends old view names somewhere that still exists", () => {
    expect(parseActivityTab("timeline")).toBe("history");
    expect(parseActivityTab("telemetry")).toBe("tasks");
  });
});

describe("stopActionFor", () => {
  it("cancels uploads, including queue items (the engine job is aborted)", () => {
    expect(stopActionFor({ kind: "upload" })).toBe("cancel");
    expect(stopActionFor({ kind: "upload-queue" })).toBe("cancel");
    expect(stopActionFor({ kind: "fs-delete" })).toBe("cancel");
  });

  it("cancels anything with an op id via FS_OP_CANCEL", () => {
    expect(
      stopActionFor({ kind: "library-move", opId: 7, addr: "10.0.0.1:9120" }),
    ).toBe("cancel");
  });

  it("only stops watching a download, whose engine job has no cancel", () => {
    expect(stopActionFor({ kind: "download" })).toBe("stop-watching");
  });

  it("offers nothing for an op it can't stop", () => {
    expect(stopActionFor({ kind: "library-launch" })).toBeNull();
    expect(stopActionFor({ kind: "library-install" })).toBeNull();
  });
});
