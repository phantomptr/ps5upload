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

  describe("Sony launcher error codes (added 2.2.32)", () => {
    it("8094000F → no profile selected", () => {
      expect(humanizePs5Error("launch_sony_error_0x8094000f")).toMatch(
        /no profile selected/i,
      );
    });
    it("8094000C → not registered", () => {
      expect(humanizePs5Error("launch_sony_error_0x8094000c")).toMatch(
        /title isn't registered/i,
      );
    });
    it("80940020 → busy", () => {
      expect(humanizePs5Error("launch_sony_error_0x80940020")).toMatch(
        /launcher is busy/i,
      );
    });
    it("8094001F → corrupted data", () => {
      expect(humanizePs5Error("launch_sony_error_0x8094001f")).toMatch(
        /corrupted/i,
      );
    });
    it("unknown launcher code falls back to generic 'returned 0xN' copy", () => {
      const out = humanizePs5Error("launch_sony_error_0x80940099");
      expect(out).toMatch(/launcher returned/i);
      expect(out).toMatch(/0x80940099/);
    });
    it("invalid title-id error", () => {
      expect(humanizePs5Error("launch_title_id_invalid")).toMatch(
        /title id doesn't look valid/i,
      );
    });
  });

  describe("Mount errors (added 2.2.32)", () => {
    it("missing image file", () => {
      expect(humanizePs5Error("fs_mount_image_not_a_file")).toMatch(
        /can't find that file/i,
      );
    });
    it("unsupported format", () => {
      expect(humanizePs5Error("fs_mount_unsupported_format")).toMatch(
        /\.ffpkg.*\.exfat/i,
      );
    });
    it("source unstable (still being written)", () => {
      expect(
        humanizePs5Error(
          "fs_mount_source_unstable: image modified 1 s ago (<3 s); wait for the upload to settle",
        ),
      ).toMatch(/wait.*upload/i);
    });
    it("path not allowed", () => {
      expect(humanizePs5Error("fs_mount_path_not_allowed")).toMatch(
        /doesn't allow mounts/i,
      );
    });
    it("attach failed", () => {
      expect(
        humanizePs5Error(
          "fs_mount_attach_failed: lvd=Permission denied md=Operation not permitted",
        ),
      ).toMatch(/couldn't attach.*re-uploading/i);
    });
    it("dev node missing", () => {
      expect(humanizePs5Error("fs_mount_dev_node_missing")).toMatch(
        /Reboot the PS5/i,
      );
    });
  });

  describe("BGFT install error codes (added 2.2.32)", () => {
    it("0x80990088 → already installed", () => {
      expect(humanizePs5Error("BGFT err 0x80990088 already installed")).toMatch(
        /already installed/i,
      );
    });
    it("0x80990085 → defrag space", () => {
      expect(humanizePs5Error("BGFT err 0x80990085 needs defrag")).toMatch(
        /defragmented free space/i,
      );
    });
  });
});
