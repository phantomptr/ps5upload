import { describe, expect, it } from "vitest";

import { humanizePs5Error } from "./humanizeError";

describe("humanizePs5Error", () => {
  it("returns empty input unchanged", () => {
    expect(humanizePs5Error("")).toBe("");
  });

  it("passes through unrecognized messages as-is", () => {
    // Passthrough matters — we never want to silently rewrite a
    // message we don't understand, because that hides diagnostic
    // signal from bug reports.
    expect(humanizePs5Error("some brand new error")).toBe(
      "some brand new error",
    );
  });

  it("surfaces local source-walk failures verbatim", () => {
    const raw = "can't read source folder: /Users/me/game";
    expect(humanizePs5Error(raw)).toBe(raw);
  });

  describe("mid-transfer network errors", () => {
    it.each([
      "read frame header failed",
      "unexpected EOF",
      "unexpectedeof on read",
      "connection reset by peer",
      "broken pipe",
    ])("maps %s to the reload hint", (raw) => {
      expect(humanizePs5Error(raw)).toMatch(
        /PS5 stopped responding.*Reload the payload/i,
      );
    });
  });

  describe("PS5 unreachable", () => {
    it("maps mgmt-port refused → mgmt hint", () => {
      expect(humanizePs5Error("connect to 192.168.1.5:9114: refused")).toMatch(
        /management service/i,
      );
    });
    it("maps transfer-port refused → transfer hint", () => {
      expect(
        humanizePs5Error("connect to 192.168.1.5:9113: refused"),
      ).toMatch(/file transfer/i);
    });
  });

  describe("filesystem errors", () => {
    it("maps EACCES → destination-write hint", () => {
      expect(humanizePs5Error("EACCES: permission denied on /data")).toMatch(
        /PS5 refused to write/i,
      );
    });
    it("maps fs_mkdir_permission_denied → destination-write hint", () => {
      expect(humanizePs5Error("fs_mkdir_permission_denied")).toMatch(
        /PS5 refused to write/i,
      );
    });
    it("does NOT match unrelated 'permission' strings", () => {
      // The earlier regex used /permission/i which matched
      // everything. Regression guard: these must NOT hit the
      // destination-write hint.
      const generic = "IPC permission error during updater check";
      expect(humanizePs5Error(generic)).toBe(generic);
    });
    it("maps ENOSPC → disk-full hint", () => {
      expect(humanizePs5Error("ENOSPC: no space left on device")).toMatch(
        /storage is full/i,
      );
    });
  });

  describe("firmware/Sony-API availability", () => {
    it("maps getmntinfo failure → retry hint", () => {
      expect(
        humanizePs5Error(
          "payload rejected FS_LIST_VOLUMES: fs_list_volumes_getmntinfo_failed",
        ),
      ).toMatch(/try again in a second/i);
    });
    it("maps sqlite_unavailable → library-feature hint", () => {
      expect(humanizePs5Error("sqlite_unavailable")).toMatch(
        /Title-registration lookups/i,
      );
    });
    it("maps launch_service_unavailable → Sony-service hint", () => {
      expect(humanizePs5Error("launch_service_unavailable")).toMatch(
        /Sony service that isn't exported/i,
      );
    });
  });

  describe("generic payload-rejection extraction", () => {
    it("extracts the reason tail from a 'payload rejected' frame", () => {
      expect(
        humanizePs5Error(
          "payload rejected FS_LIST_DIR(/data): fs_list_dir_opendir_errno_13",
        ),
      ).toMatch(/PS5 rejected the request: fs_list_dir_opendir_errno_13/);
    });
    it("handles the no-path form", () => {
      expect(humanizePs5Error("payload rejected COMMIT_TX: tx_not_found")).toMatch(
        /PS5 rejected the request: tx_not_found/,
      );
    });
  });
});
