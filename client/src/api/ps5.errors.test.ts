import { describe, expect, it } from "vitest";

import {
  UploadJobError,
  describeSpaceRefusal,
  humanizeJobErrorReason,
  volumeAllocatableBytes,
  volumeForPath,
  type Volume,
} from "./ps5";

/**
 * Tests for the Phase B error-humanization layer and Phase C
 * volume-by-path matcher. These don't touch the network — they're
 * pure data transforms — so they live in a dedicated test file rather
 * than the api/ps5.ts file itself (which is a barrel of mostly Tauri-
 * invoke wrappers).
 */

describe("humanizeJobErrorReason", () => {
  it("returns null for undefined / null / unknown reasons", () => {
    expect(humanizeJobErrorReason(undefined)).toBeNull();
    expect(humanizeJobErrorReason("")).toBeNull();
    expect(humanizeJobErrorReason("totally_made_up_token")).toBeNull();
  });

  it("explains ava1_stalled (the source stopped sending data)", () => {
    expect(humanizeJobErrorReason("ava1_stalled")).toMatch(
      /stopped making progress.*Retry.*source drive or network share/,
    );
  });

  it("recognizes ENOSPC-shaped errors", () => {
    // The two write-side ENOSPC variants: payload's writer-thread
    // I/O error mid-stream, and the upfront fs_write_failed_errno_28.
    // Both should surface the "free up space" guidance.
    expect(humanizeJobErrorReason("direct_writer_io_error")).toMatch(
      /free space|out of free/i,
    );
    expect(humanizeJobErrorReason("fs_write_failed_errno_28")).toMatch(
      /space|too big/i,
    );
  });

  it("generalizes any fs_write_failed_errno_<N> (not just 27/28)", () => {
    // The payload now reports the actual errno from a mid-transfer write
    // failure (e.g. 5=EIO, 13=EACCES) instead of silently dropping the
    // connection. Unknown codes must still get actionable write-failure
    // guidance, not fall through to null (which showed the misleading
    // "PS5 stopped responding" message).
    const msg = humanizeJobErrorReason("fs_write_failed_errno_5");
    expect(msg).not.toBeNull();
    expect(msg).toMatch(/write|destination|drive|retry/i);
    // Bare fallback (errno unavailable, e.g. the threaded writer path).
    expect(humanizeJobErrorReason("fs_write_failed")).toMatch(
      /write|destination|drive|retry/i,
    );
  });

  it("recognizes pre-flight insufficient-space reason", () => {
    // Phase C surfaces this BEFORE the upload starts; humanization
    // must explicitly mention "destination drive" so the user knows
    // it's a destination issue, not a transport problem.
    const msg = humanizeJobErrorReason("preflight_insufficient_space");
    expect(msg).toMatch(/destination drive/i);
    expect(msg).toMatch(/retry|free up/i);
  });

  it("does not tell a user to free PS5 storage for a full game image", () => {
    const msg = humanizeJobErrorReason("preflight_insufficient_space_image");
    expect(msg).toMatch(/inside a game image/i);
    expect(msg).toMatch(/doesn't help/i);
  });

  it("recognizes path-denied errors", () => {
    // Three payload tokens funnel into one "use an allowed path" hint.
    // Pinning the recognized set so a future payload addition that
    // forgets to wire its token surfaces as a missing-coverage test.
    const tokens = [
      "fs_delete_path_not_allowed",
      "fs_mkdir_path_not_allowed",
      "fs_list_dir_path_denied",
    ];
    for (const t of tokens) {
      const msg = humanizeJobErrorReason(t);
      expect(msg, `expected humanization for token ${t}`).not.toBeNull();
      expect(msg).toMatch(/path|allowed|access/i);
    }
  });

  it("recognizes system-file-read-blocked and suggests the toggle", () => {
    // The read path is distinct from the destructive path-denied case:
    // the fix is to enable the Settings toggle, not to change the path.
    const msg = humanizeJobErrorReason("fs_read_path_not_allowed");
    expect(msg).not.toBeNull();
    expect(msg).toMatch(/system partition|Allow downloading/i);
  });

  it("recognizes the protocol-corruption case", () => {
    const msg = humanizeJobErrorReason("direct_tx_corrupt");
    expect(msg).toMatch(/protocol|corruption|restart/i);
  });
});

describe("volumeAllocatableBytes", () => {
  const base: Volume = {
    path: "/data",
    fs_type: "bfs",
    total_bytes: 1_000,
    free_bytes: 400,
    writable: true,
  };

  it("prefers reserve-aware payload telemetry", () => {
    expect(volumeAllocatableBytes({ ...base, allocatable_bytes: 250 })).toBe(250);
  });

  it("keeps older payload responses compatible and clamps invalid values", () => {
    expect(volumeAllocatableBytes(base)).toBe(400);
    expect(volumeAllocatableBytes({ ...base, allocatable_bytes: -1 })).toBe(0);
  });
});

describe("UploadJobError", () => {
  it("constructs with message + structured reason/detail", () => {
    const e = new UploadJobError(
      "transfer failed",
      "direct_writer_io_error",
      "writer thread reported a disk write error",
    );
    expect(e).toBeInstanceOf(Error);
    expect(e).toBeInstanceOf(UploadJobError);
    expect(e.name).toBe("UploadJobError");
    expect(e.message).toBe("transfer failed");
    expect(e.reason).toBe("direct_writer_io_error");
    expect(e.detail).toBe("writer thread reported a disk write error");
  });

  it("allows reason/detail to be undefined", () => {
    // For non-payload-origin errors (local I/O etc.) the runner
    // should still be able to construct an UploadJobError with just
    // the message — `instanceof UploadJobError` check in the runner
    // stays simple.
    const e = new UploadJobError("queue stopped");
    expect(e.reason).toBeUndefined();
    expect(e.detail).toBeUndefined();
  });

  it("is catchable via instanceof to lift structured fields", () => {
    // Mirrors how the upload runner catches it.
    function thrower(): never {
      throw new UploadJobError("bad", "preflight_insufficient_space", "/data short");
    }
    try {
      thrower();
    } catch (e) {
      const reason = e instanceof UploadJobError ? e.reason : null;
      const detail = e instanceof UploadJobError ? e.detail : null;
      expect(reason).toBe("preflight_insufficient_space");
      expect(detail).toBe("/data short");
      return;
    }
    expect.fail("UploadJobError was not thrown");
  });
});

describe("volumeForPath (longest-prefix match)", () => {
  function v(
    path: string,
    freeBytes = 0,
    isPlaceholder = false,
  ): Volume {
    return {
      path,
      fs_type: "ufs",
      total_bytes: 0,
      free_bytes: freeBytes,
      writable: true,
      is_placeholder: isPlaceholder,
      mount_from: "",
      source_image: "",
    };
  }

  it("picks the deepest matching mount for a nested destination", () => {
    const vlist = [v("/"), v("/data"), v("/mnt/ext0")];
    expect(volumeForPath(vlist, "/mnt/ext0/games/big.pkg")?.path).toBe(
      "/mnt/ext0",
    );
  });

  it("matches the exact volume path", () => {
    expect(volumeForPath([v("/data")], "/data")?.path).toBe("/data");
  });

  it("does NOT confuse /data with /database/", () => {
    // Regression for the prefix bug: a path like "/database/foo"
    // matched against volume "/data" must fall through (not match),
    // because the prefix has to end on a path-segment boundary.
    expect(volumeForPath([v("/data")], "/database/foo")).toBeNull();
  });

  it("falls through to / when the deeper mounts don't match", () => {
    const vlist = [v("/"), v("/data"), v("/mnt/ext0")];
    expect(volumeForPath(vlist, "/anywhere/else")?.path).toBe("/");
  });

  it("returns null when nothing matches and there's no / mount", () => {
    expect(volumeForPath([v("/data")], "/elsewhere")).toBeNull();
  });

  it("handles volume path with trailing slash", () => {
    // Defensive: a payload that started returning `path: "/data/"`
    // (with trailing slash) shouldn't break the matcher.
    expect(volumeForPath([v("/data/")], "/data/foo")?.path).toBe("/data/");
  });

  it("returns null on empty list", () => {
    expect(volumeForPath([], "/whatever")).toBeNull();
  });

  it("skips entries with empty path string", () => {
    // Defensive against malformed payload responses where a volume
    // record has path:"" — must not match every dest.
    const vlist: Volume[] = [
      { ...v("/data"), path: "" },
      v("/data"),
    ];
    expect(volumeForPath(vlist, "/data/foo")?.path).toBe("/data");
  });
});

describe("describeSpaceRefusal", () => {
  const base = {
    volume: "/data/x",
    need_bytes: 2048,
    avail_bytes: 1024,
    free_bytes: 3072,
    reserve_bytes: 2048,
    promised_bytes: 0,
    reused_bytes: 0,
    image: false,
    late: false,
  };

  it("fills every figure on a drive", () => {
    const m = describeSpaceRefusal(base);
    expect(m).toBe(
      "/data/x needs 2.00 KiB more for this upload, but only 1.00 KiB can be used (3.00 KiB free, 2.00 KiB kept back).",
    );
  });

  it("explains what an image can take and how to make room in it", () => {
    // The Discord report: a game image with 200 MiB free refused a 211 MiB file.
    const m = describeSpaceRefusal({
      ...base,
      volume: "/mnt/shadowmnt/G_ca51a0d7/data.bin",
      need_bytes: 221_531_975,
      avail_bytes: 200 * 1024 * 1024,
      reserve_bytes: 0,
      image: true,
    });
    expect(m).toContain("211 MiB");
    expect(m).toContain("200 MiB");
    expect(m).toContain("Delete the old file first");
    expect(m).not.toContain("kept back");
    expect(m).not.toContain("{");
  });

  it("adds what other uploads hold and what is reused only when there is some", () => {
    const m = describeSpaceRefusal({ ...base, promised_bytes: 1024, reused_bytes: 4096 });
    expect(m).toContain("1.00 KiB is set aside for other uploads");
    expect(m).toContain("4.00 KiB of this upload is already on the console");
    expect(describeSpaceRefusal(base)).not.toContain("set aside");
    expect(describeSpaceRefusal(base)).not.toContain("already on the console");
  });

  it("does not blame internal storage for a late refusal on an image", () => {
    const drive = describeSpaceRefusal({ ...base, late: true });
    expect(drive).toContain("internal storage holds back");
    const image = describeSpaceRefusal({ ...base, late: true, image: true });
    expect(image).not.toContain("internal storage");
    expect(image).toContain("The image ran out of room");
  });
});

describe("humanizeJobErrorReason i18n", () => {
  it("returns the English text when no locale is loaded", () => {
    // trStatic falls back to the English literal at the call site, so a key
    // missing from a locale degrades to the pre-i18n behaviour rather than
    // leaking a raw key like "joberr.tx_table_full" into the UI.
    const msg = humanizeJobErrorReason("tx_table_full");
    expect(msg).toBeTruthy();
    expect(msg).not.toContain("joberr.");
  });

  it("never leaks a translation key for any known reason", () => {
    const reasons = [
      "preflight_insufficient_space",
      "preflight_insufficient_space_image",
      "direct_staged_file_missing",
      "direct_writer_io_error",
      "direct_tx_corrupt",
      "packed_unsupported",
      "size_mismatch",
      "shards_incomplete",
      "spool_apply_failed",
      "fs_delete_path_not_allowed",
      "fs_mkdir_path_not_allowed",
      "fs_list_dir_path_denied",
      "fs_read_path_not_allowed",
      "tx_table_full",
      "zip_unsupported",
      "7z_unsupported",
      "7z_unsupported_layout",
      "ava1_7z_unsupported",
      "ava1_7z_unsupported_layout",
      "rar_unsupported",
      "ava1_rar_unsupported",
      "fs_write_failed_errno_28",
      "fs_write_failed_errno_27",
      "fs_write_failed",
    ];
    for (const r of reasons) {
      const msg = humanizeJobErrorReason(r);
      expect(msg, `reason ${r}`).toBeTruthy();
      expect(msg, `reason ${r}`).not.toContain("joberr.");
    }
  });

  it("tells the user what to do about an archive the uploader cannot stream", () => {
    for (const [reason, mustMention] of [
      ["zip_unsupported", /\.zip/],
      ["7z_unsupported", /\.7z/],
      ["7z_unsupported_layout", /Re-pack|extract/i],
      ["ava1_7z_unsupported_layout", /Re-pack|extract/i],
      ["rar_unsupported", /\.rar/],
    ] as const) {
      const msg = humanizeJobErrorReason(reason)!;
      expect(msg, reason).toMatch(mustMention);
      expect(msg, reason).toMatch(/[Ee]xtract/);
      expect(msg, reason).not.toContain("joberr.");
    }
    // The layout failure is its own message, not the generic 7z one.
    expect(humanizeJobErrorReason("7z_unsupported_layout")).not.toBe(
      humanizeJobErrorReason("7z_unsupported"),
    );
  });

  it("still returns null for an unknown reason so callers fall back", () => {
    expect(humanizeJobErrorReason("something_new_from_a_newer_payload")).toBeNull();
  });

  it("maps the repeat-commit reason to its own message, not the rename one", () => {
    // Regression: the payload used to report `direct_rename_failed` with an
    // empty source path for this case, which sent users chasing a rename bug.
    const msg = humanizeJobErrorReason("direct_staged_file_missing");
    expect(msg).toMatch(/Override/i);
  });
});
