import { describe, expect, it } from "vitest";

import { partialUploadPath } from "./partialUpload";

const item = (over: Record<string, unknown>) =>
  ({
    status: "failed",
    sourceKind: "image",
    resolvedDest: "/data/homebrew/PPSA03671 Wolverine.ffpfsc",
    errorReason: "ava1_no_space",
    ...over,
  }) as Parameters<typeof partialUploadPath>[0];

describe("partialUploadPath", () => {
  it("names the partial file a single-file upload left when the drive filled", () => {
    expect(partialUploadPath(item({}))).toBe("/data/homebrew/PPSA03671 Wolverine.ffpfsc.ava-part");
    expect(partialUploadPath(item({ sourceKind: "file", errorReason: "preflight_insufficient_space" }))).toBe(
      "/data/homebrew/PPSA03671 Wolverine.ffpfsc.ava-part",
    );
  });
  it("is null for other failures, folders and archives", () => {
    expect(partialUploadPath(item({ errorReason: "ava1_connect" }))).toBeNull();
    expect(partialUploadPath(item({ sourceKind: "folder" }))).toBeNull();
    expect(partialUploadPath(item({ sourceKind: "archive" }))).toBeNull();
    expect(partialUploadPath(item({ status: "done" }))).toBeNull();
  });
});
