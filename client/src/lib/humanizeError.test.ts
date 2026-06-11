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

  describe("RAR archive errors", () => {
    it("maps rar_password_required → password-protected prompt", () => {
      expect(humanizePs5Error("rar_password_required")).toMatch(
        /password-protected/i,
      );
    });
    it("maps rar_password_wrong → wrong-password copy", () => {
      expect(humanizePs5Error("rar_password_wrong")).toMatch(/wrong password/i);
    });
    it("maps the Android 501 body → unsupported copy", () => {
      expect(humanizePs5Error("RAR is not supported on this build")).toMatch(
        /aren't supported on this build|\.zip and \.7z only/i,
      );
    });
    it("maps an unsafe entry path → safety refusal copy", () => {
      expect(
        humanizePs5Error(
          'rar contains an unsafe or invalid entry path: "../escape"',
        ),
      ).toMatch(/outside the destination|refused for safety/i);
    });
    it.each([
      "open rar: archive open error",
      "read rar header: bad data",
      "extract rar entry: corrupt",
      "skip rar entry: failed",
    ])("maps generic unrar failure %s → multi-part guidance", (raw) => {
      expect(humanizePs5Error(raw)).toMatch(
        /FIRST part|multi-part|same folder/i,
      );
    });
    it("does not let the multi-part rule swallow unrelated text", () => {
      // 'rar' as a substring of other words must not trigger the open-failed
      // branch — it keys on the specific "open rar:" etc. prefixes.
      expect(humanizePs5Error("preparation already started")).toBe(
        "preparation already started",
      );
    });
  });

  describe("manifest_invalid (multi-file BEGIN_TX rejection)", () => {
    it("maps the raw BeginTx rejection to an actionable hint", () => {
      const raw = "BeginTx rejected (Error): manifest_invalid";
      const out = humanizePs5Error(raw);
      expect(out).not.toBe(raw);
      expect(out).toMatch(/rejected the list of files/i);
      expect(out).toMatch(/Send payload|rename/i);
    });
    it("passes the engine's pre-flight length error through verbatim", () => {
      // The engine names the offending file; that copy is already
      // readable, so the humanizer must not swallow it.
      const raw =
        "destination path is too long for the PS5 (640 bytes, limit 511): /data/game/very/long/path";
      expect(humanizePs5Error(raw)).toBe(raw);
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

  // Each Sony AppInstUtil code is matched in three forms (hex, decimal,
  // SCE_… symbol) so the engine's varying upstream representations all
  // route to the same actionable copy. A regression that drops one form
  // surfaces as "PS5 rejected the request: <raw>" in the UI, which is
  // exactly the silent-failure mode these tests exist to prevent.
  describe("Sony AppInstUtil error codes (0x80A2_FFXX / 0x80A3_00XX)", () => {
    it("0x80A30000 NOT_INITIALIZED → push latest payload", () => {
      expect(humanizePs5Error("0x80A30000")).toMatch(
        /Sony's installer subsystem isn't initialised/i,
      );
      expect(humanizePs5Error("err -2136862720")).toMatch(
        /Sony's installer subsystem isn't initialised/i,
      );
      expect(
        humanizePs5Error("SCE_APP_INST_UTIL_ERROR_NOT_INITIALIZED"),
      ).toMatch(/Sony's installer subsystem isn't initialised/i);
    });
    it("0x80A2FF02 NOSPACE → free up storage", () => {
      expect(humanizePs5Error("Sony rejected: 0x80A2FF02")).toMatch(
        /enough free space/i,
      );
      expect(humanizePs5Error("rc=-2136801278")).toMatch(/enough free space/i);
      expect(humanizePs5Error("SCE_APP_INSTALLER_ERROR_NOSPACE")).toMatch(
        /enough free space/i,
      );
    });
    it("0x80A2FF06 INVALID_DRM_TYPE → use Patch-DRM register", () => {
      expect(humanizePs5Error("0x80A2FF06")).toMatch(
        /Patch DRM|applicationDrmType/i,
      );
      expect(humanizePs5Error("err -2136801274")).toMatch(
        /Patch DRM|applicationDrmType/i,
      );
    });
    it("0x80A2FF09 INVALID_CONTENT_TYPE → patch / DLC pkg rejected", () => {
      expect(humanizePs5Error("Sony 0x80A2FF09")).toMatch(/content type/i);
      expect(humanizePs5Error("rc=-2136801271")).toMatch(/content type/i);
    });
    it("0x80A2FF14 BUSY → wait & retry", () => {
      expect(humanizePs5Error("0x80A2FF14")).toMatch(/busy/i);
      expect(humanizePs5Error("err=-2136801260")).toMatch(/busy/i);
    });
    it("0x80A2FF15 ALREADY_INSTALLED → uninstall first", () => {
      expect(humanizePs5Error("0x80A2FF15")).toMatch(/already installed/i);
      expect(humanizePs5Error("rc -2136801259")).toMatch(/already installed/i);
    });
    it("0x80A30001 OUT_OF_MEMORY → reboot suggestion", () => {
      expect(humanizePs5Error("err 0x80A30001")).toMatch(/Reboot the PS5/i);
      expect(humanizePs5Error("err -2136862719")).toMatch(/Reboot the PS5/i);
    });

    // Regression guard: an unknown 0x80A3_xxxx code must NOT silently
    // collide with the NOT_INITIALIZED branch (which is keyed on the
    // exact 0x80A30000 code). A too-loose `0x80A3` regex would absorb
    // every code in this range and ship users wrong copy.
    it("unknown 0x80A3_xxxx code falls through to generic copy", () => {
      const raw = "Sony returned 0x80A3FFFF";
      const out = humanizePs5Error(raw);
      expect(out).not.toMatch(/Sony's installer subsystem isn't initialised/i);
    });
  });
});
