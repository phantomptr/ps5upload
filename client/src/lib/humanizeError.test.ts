import { describe, expect, it } from "vitest";

import { caughtText, humanizeCaught, humanizePs5Error } from "./humanizeError";
import en from "../i18n/locales/en";

describe("humanizePs5Error", () => {
  it("words this computer's lack of room in the user's language", () => {
    expect(humanizePs5Error("engine HTTP 507: Not enough space: needs 12.5 GiB, 1.0 GiB free.")).toBe(
      "Not enough free space on this computer: it needs 12.5 GiB, and 1.0 GiB is free. Free some space and try again.",
    );
  });

  it("says a link's file changed in the user's language", () => {
    expect(
      humanizePs5Error(
        "the file at this link changed while it was being read, so what was read so far can't be finished with it; start again",
      ),
    ).toBe(en.err_link_changed);
  });

  it("says the next step for a pairing the PS5 refuses as pending (80108b03)", () => {
    const m = humanizePs5Error("registration failed (HTTP 403): anything (reason 80108B03)");
    expect(m).toBe(en.err_pair_pending);
    expect(m).toContain("Enable Remote Play");
    expect(m).toContain("Pair again");
    expect(m).toContain("Accounts › Users");
  });

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
    it("maps the helper's single port (:9120) refused → reachability hint", () => {
      expect(humanizePs5Error("connect to 192.168.1.5:9120: refused")).toMatch(
        /Can't reach your PS5/i,
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
        /aren't supported on phones/i,
      );
    });
    it("maps an unsafe entry path → safety refusal copy", () => {
      expect(
        humanizePs5Error(
          'rar contains an unsafe or invalid entry path: "../escape"',
        ),
      ).toMatch(/outside the destination|refused for safety/i);
    });
    it("maps a refused zip entry path → extract-and-upload copy", () => {
      expect(
        humanizePs5Error(
          'zip contains an unsafe or invalid entry path: "Game\\sce_sys\\param.json"',
        ),
      ).toMatch(/upload the folder instead/i);
    });
    /* The engine now checks the volume set itself, so a generic failure
     * means the parts really are all present. Telling the user to "select
     * the FIRST part and put every volume in one folder" was the old copy,
     * and it was reported by a user who had done exactly that — the advice
     * was unactionable, so the message must no longer give it. */
    it.each([
      "open rar: archive open error",
      "read rar header: bad data",
      "extract rar entry: corrupt",
      "skip rar entry: failed",
    ])("maps generic unrar failure %s → damaged/unsupported copy", (raw) => {
      const msg = humanizePs5Error(raw);
      expect(msg).toMatch(/damaged|doesn't support|re-download/i);
      expect(msg).not.toMatch(/select the FIRST part/i);
      // The library's reason stays visible, so a report can be diagnosed.
      expect(msg).toContain(raw);
    });
    it("names the specific volume that is missing", () => {
      const msg = humanizePs5Error("rar_missing_volume: game.part2.rar");
      expect(msg).toMatch(/game\.part2\.rar/);
      expect(msg).toMatch(/incomplete|missing/i);
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
        /title isn't on its home screen/i,
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
    it("launch_no_user → sign in on the PS5 first", () => {
      expect(
        humanizePs5Error("payload rejected APP_LAUNCH: launch_no_user"),
      ).toMatch(/Sign in on the PS5 first, then press Play again/);
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

  describe("System file read blocked by allowlist", () => {
    it("maps fs_read_path_not_allowed to the settings toggle hint", () => {
      expect(humanizePs5Error("fs_read_path_not_allowed")).toMatch(
        /Allow downloading system files/i,
      );
    });
    it("matches the token embedded in a longer error string", () => {
      expect(
        humanizePs5Error(
          "payload rejected FS_READ(/system/common/lib/libkernel.sprx): fs_read_path_not_allowed",
        ),
      ).toMatch(/system partition/i);
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

describe("0x80B21104", () => {
  it("says the console would not take this package, not a generic PlayGo error", () => {
    expect(humanizePs5Error("The PS5 declined the install. (0x80b21104)")).toContain("would not accept this package");
  });
});

describe("install and launch advice names today's app", () => {
  // These used to send people to FTP and to the retired Library screen.
  const cases: [string, string][] = [
    ["launch, unknown code", "launch_sony_error_0x80941234"],
    ["installer not initialised", "0x80A30000"],
    ["DRM type", "0x80A2FF06"],
    ["BGFT not loadable", "BGFT symbol missing: sceBgftServiceIntDownloadRegisterTaskByStorageEx"],
    ["ENOENT, not a DLC", "sceKernelOpen failed 0x80020002"],
  ];
  for (const [what, raw] of cases) {
    it(what, () => {
      const out = humanizePs5Error(raw);
      expect(out).not.toMatch(/FTP|Library/);
      expect(out).not.toBe(raw);
    });
  }
  it("points at the game's page in Games where a folder or image can be registered", () => {
    expect(humanizePs5Error("launch_sony_error_0x80941234")).toContain("the game's page in Games");
    expect(humanizePs5Error("0x80A2FF06")).toContain("the game's page in Games");
  });
});

describe("archive extract causes", () => {
  const cases: [string, string, Record<string, string>?][] = [
    ["fs_extract_no_space: needs 9 bytes, 1 free", "err_extract_no_space"],
    ["fs_extract_destination_exists", "err_extract_exists"],
    ["fs_extract_busy", "err_extract_busy"],
    ["fs_extract_password_required", "err_extract_password_required"],
    ["fs_extract_wrong_password", "err_extract_wrong_password"],
    ["fs_extract_rar_encrypted", "err_extract_rar_encrypted"],
    ["fs_extract_unsupported_method: PPMd", "err_extract_unsupported_method"],
    ["fs_extract_corrupt: CRC failed in a/b", "err_extract_corrupt"],
    ["fs_extract_truncated", "err_extract_corrupt"],
    ["fs_extract_not_archive", "err_extract_not_archive"],
    ["fs_extract_missing_volume: g.part2.rar", "err_extract_missing_volume", { name: "g.part2.rar" }],
    ["fs_extract_loader_unreachable", "err_extract_loader"],
    ["fs_extract_died", "err_extract_died"],
    ["fs_extract_not_started", "err_extract_died"],
    ["fs_extract_no_memory", "err_extract_no_memory"],
    ["fs_extract_write_failed: EIO", "err_extract_write_failed"],
    ["fs_extract_open_failed", "err_extract_open_failed"],
    ["fs_extract_path_not_allowed", "err_extract_path"],
    ["fs_extract_bad_dest", "err_extract_path"],
  ];
  for (const [raw, key, vars] of cases) {
    it(raw, () => {
      let want = (en as Record<string, string>)[key];
      expect(want, key).toBeTruthy();
      for (const [k, v] of Object.entries(vars ?? {})) want = want.replace(`{${k}}`, v);
      expect(humanizePs5Error(raw)).toBe(want);
    });
  }
  it("leaves an unknown extract cause as it is", () => {
    expect(humanizePs5Error("fs_extract_something_new")).toBe("fs_extract_something_new");
  });
});

describe("a React render loop in the queue", () => {
  it("says the transfer may have finished instead of React's text", () => {
    const out = humanizePs5Error(
      "Minified React error #185; visit https://react.dev/errors/185 for the full message",
    );
    expect(out).toContain("may have finished");
    expect(out).not.toContain("react.dev");
  });
});

describe("caughtText", () => {
  it("reads every shape a catch can receive", () => {
    expect(caughtText(new Error("boom"))).toBe("boom");
    expect(caughtText("plain")).toBe("plain");
    expect(caughtText({ message: "from a message" })).toBe("from a message");
    expect(caughtText({ error: "from an error" })).toBe("from an error");
    expect(caughtText({ code: 5 })).toBe('{"code":5}');
    expect(caughtText(undefined)).toBe("undefined");
  });
  it("never prints [object Object]", () => {
    const cycle: Record<string, unknown> = {};
    cycle.self = cycle;
    expect(caughtText({ code: 1 })).not.toContain("[object");
    expect(typeof caughtText(cycle)).toBe("string");
  });
});

describe("humanizeCaught", () => {
  it("gives the sentence for a known code and the text for an unknown one", () => {
    expect(humanizeCaught(new Error("fs_unmount_busy"))).not.toContain("fs_unmount_busy");
    expect(humanizeCaught(new Error("something else"))).toBe("something else");
  });
});
