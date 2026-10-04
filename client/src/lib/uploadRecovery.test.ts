import { describe, expect, it } from "vitest";
import {
  AUTO_RECOVER_BACKOFF_MS,
  autoRecoverBackoffMs,
  isAutoRecoverable,
  MAX_AUTO_RECOVER_ATTEMPTS,
  PostUploadStepError,
  refineHelperReason,
  shouldAutoRecover,
} from "./uploadRecovery";

describe("isAutoRecoverable", () => {
  it("recovers the payload-crash / connection-drop class", () => {
    // The whole point: the multistream-crash bug report's signature.
    expect(isAutoRecoverable(null, "connect to 192.168.1.60:9113 ... refused"))
      .toBe(true);
    expect(isAutoRecoverable(null, "write frame split: Broken pipe")).toBe(true);
    expect(isAutoRecoverable(null, "Connection reset by peer")).toBe(true);
    expect(isAutoRecoverable(null, "timed out")).toBe(true);
    expect(isAutoRecoverable(null, "unexpected end of file")).toBe(true);
    // Payload went to rest mode mid-apply → re-deploy + resume fixes it.
    expect(isAutoRecoverable("spool_apply_failed", "rest mode")).toBe(true);
    // Old payload rejecting packed shards → re-deploy sends the current ELF.
    expect(isAutoRecoverable("packed_unsupported", "")).toBe(true);
  });

  it("does NOT recover fatal payload reasons (retry can't help)", () => {
    expect(isAutoRecoverable("fs_write_failed_errno_28", "")).toBe(false); // ENOSPC
    expect(isAutoRecoverable("fs_write_failed_errno_27", "")).toBe(false); // EFBIG
    expect(isAutoRecoverable("preflight_insufficient_space", "")).toBe(false);
    expect(isAutoRecoverable("direct_writer_io_error", "")).toBe(false);
    expect(isAutoRecoverable("fs_open_path_not_allowed", "")).toBe(false);
    expect(isAutoRecoverable("tx_table_full", "")).toBe(false);
    expect(isAutoRecoverable("direct_tx_corrupt", "")).toBe(false);
  });

  it("never auto-recovers a post-commit failure", () => {
    // Every byte landed and the console refused the commit/rename. Both
    // typed reasons travel from ps5upload_ava1::PostCommitKind and are
    // terminal: the destination is taken, so re-running re-uploads bytes
    // that are already durable.
    expect(isAutoRecoverable("ava1_commit_exists", "")).toBe(false);
    expect(isAutoRecoverable("ava1_commit_cross_device", "")).toBe(false);
  });

  it.each([
    "ava1_exists",
    "ava1_cross_device",
    "ava1_refused_1",
    "ava1_refused_2",
    "ava1_refused_3",
    "ava1_refused_4",
    "ava1_refused_5",
    "ava1_refused_6",
    "ava1_local_io",
    "ava1_bad_manifest",
    "ava1_copy_failed",
    "ava1_not_paired",
    "ava1_wrong_console",
    "ava1_no_identity",
    "ava1_no_space",
    "ava1_not_allowed",
  ])("does NOT recover AVA1 refusal %s (retrying cannot change it)", (r) => {
    expect(isAutoRecoverable(r, "")).toBe(false);
  });

  it.each(["not_paired", "password_needed"])(
    "does NOT recover the bare token %s (a person must act: pair, or type the password)",
    (r) => {
      expect(isAutoRecoverable(r, "")).toBe(false);
    },
  );

  it("DOES recover helper_not_ava1: re-sending the helper is the recovery", () => {
    expect(isAutoRecoverable("helper_not_ava1", "")).toBe(true);
  });

  it.each([
    "zip_unsupported",
    "7z_unsupported",
    "7z_unsupported_layout",
    "ava1_7z_unsupported",
    "ava1_7z_unsupported_layout",
    "ava1_7z_corrupt",
    "ava1_7z_encrypted",
    "ava1_zip_corrupt",
    "rar_unsupported",
    "ava1_rar_unsupported",
  ])("does NOT recover the unreadable-archive failure %s (the same archive fails the same way)", (r) => {
    expect(isAutoRecoverable(r, "")).toBe(false);
  });

  it.each([
    "ava1_rar_password_required",
    "ava1_rar_password_wrong",
    "ava1_rar_corrupt",
    "ava1_rar_missing_volume",
    "ava1_rar_reordered",
    "ava1_rar_failed",
    "ava1_rar_some_future_reason",
    "rar_password_required",
    "rar_password_wrong",
  ])("does NOT recover RAR failure %s (same archive and password fail again)", (r) => {
    expect(isAutoRecoverable(r, "")).toBe(false);
  });

  it.each([
    "ava1_refused_7", // internal
    "ava1_refused_8", // busy
    "ava1_refused_11", // unknown job (console restarted)
    "ava1_refused_12", // io
    "ava1_refused_13", // verify
    "ava1_refused_17", // credit
    "ava1_refused_65535", // unknown code
    "ava1_refused_",
  ])("keeps the transient or unknown refusal %s retryable", (r) => {
    expect(isAutoRecoverable(r, "")).toBe(true);
  });

  it("recovers ava1_stalled: the receiver ended a job whose source stopped sending data (review 006)", () => {
    expect(isAutoRecoverable("ava1_stalled", "")).toBe(true);
  });

  it("recovers zip_read_error: an I/O failure reading the archive is transient", () => {
    expect(isAutoRecoverable("zip_read_error", "")).toBe(true);
  });

  it("recovers ava1_unreachable: the payload may need a re-deploy", () => {
    expect(isAutoRecoverable("ava1_unreachable", "")).toBe(true);
  });

  it("still auto-recovers an unknown AVA1 reason", () => {
    // The post-commit entry is a prefix on ava1_commit_, not a blanket
    // ban on the ava1_ prefix — a future unrelated reason must keep the
    // default (recoverable, bounded by the attempt cap).
    expect(isAutoRecoverable("ava1_something_new", "")).toBe(true);
  });

  it("does NOT recover fatal LOCAL errors (no payload reason)", () => {
    expect(isAutoRecoverable(null, "No such file or directory (os error 2)"))
      .toBe(false);
    expect(isAutoRecoverable(null, "permission denied")).toBe(false);
    expect(isAutoRecoverable(null, "No space left on device")).toBe(false);
  });

  it("ignores fatal-looking words in a payload detail when a non-fatal reason is present", () => {
    // A transport reason whose detail incidentally mentions a file path
    // containing 'not found' must still recover — we trust the structured
    // reason over the free-text message.
    expect(
      isAutoRecoverable(
        "spool_apply_failed",
        "could not find spool entry; file not found in tmp",
      ),
    ).toBe(true);
  });

  it("defaults unknown reasons to recoverable (bounded by the attempt cap)", () => {
    expect(isAutoRecoverable("some_brand_new_reason", "")).toBe(true);
    expect(isAutoRecoverable(null, "an unclassified transient blip")).toBe(true);
  });

  // Real failure strings captured from a payload crash mid-upload on a Fat PS5
  // (192.168.86.99) — both surface with error_reason=null, so they MUST be
  // classified via the message and MUST be recoverable, or the auto-resume
  // feature is useless for the exact case it exists to handle.
  it("recovers the actual hardware crash signatures (regression)", () => {
    expect(
      isAutoRecoverable(
        null,
        "transfer_file_list gave up after 2 retries. Prior: attempt 0: write frame split | attempt 1: read frame header: connect to 192.168.86.99:9113: Connection refused (os error 61)",
      ),
    ).toBe(true);
    // Crash landed during a stream's commit phase.
    expect(isAutoRecoverable(null, "CommitTx rejected (Error): tx_not_active")).toBe(
      true,
    );
  });
});

describe("autoRecoverBackoffMs", () => {
  it("escalates then clamps to the last step", () => {
    expect(autoRecoverBackoffMs(0)).toBe(AUTO_RECOVER_BACKOFF_MS[0]);
    expect(autoRecoverBackoffMs(1)).toBe(AUTO_RECOVER_BACKOFF_MS[1]);
    expect(autoRecoverBackoffMs(2)).toBe(AUTO_RECOVER_BACKOFF_MS[2]);
    // Past the array → clamp to last (defensive; the loop never exceeds the cap).
    expect(autoRecoverBackoffMs(99)).toBe(
      AUTO_RECOVER_BACKOFF_MS[AUTO_RECOVER_BACKOFF_MS.length - 1],
    );
    expect(autoRecoverBackoffMs(-5)).toBe(AUTO_RECOVER_BACKOFF_MS[0]);
  });

  it("has a backoff entry for every recovery attempt", () => {
    expect(AUTO_RECOVER_BACKOFF_MS.length).toBe(MAX_AUTO_RECOVER_ATTEMPTS);
  });
});

describe("shouldAutoRecover", () => {
  it("never re-runs an item whose install failed after the upload committed", () => {
    // The 2026-09-08 report: the update uploaded (6.37 GB, committed), the
    // install was rejected, and the queue silently started the upload again.
    // The install message is translated, so nothing about its text can be
    // relied on — only its type.
    const installFailed = new PostUploadStepError(
      "This update couldn’t be applied because ps5upload couldn’t reach your PS5’s payload loader on port 9021…",
    );
    expect(shouldAutoRecover(installFailed, null, installFailed.message)).toBe(
      false,
    );
    // Same in a language the matcher has never seen.
    const localised = new PostUploadStepError(
      "このアップデートは適用できませんでした。",
    );
    expect(shouldAutoRecover(localised, null, localised.message)).toBe(false);
  });

  it("never re-uploads when the post-upload mount failed", () => {
    const mountFailed = new PostUploadStepError(
      "upload completed, but mount failed: Device busy (os error 16)",
    );
    expect(shouldAutoRecover(mountFailed, null, mountFailed.message)).toBe(
      false,
    );
  });

  it("still recovers a genuine transport failure", () => {
    // The case auto-recovery exists for must keep working: the payload died
    // mid-transfer and a resume picks up from the committed shards.
    const dropped = new Error("connect to 192.168.1.60:9113 ... refused");
    expect(shouldAutoRecover(dropped, null, dropped.message)).toBe(true);
    expect(shouldAutoRecover(dropped, "spool_apply_failed", "rest mode")).toBe(
      true,
    );
  });

  it("keeps deferring to the reason/message policy for plain errors", () => {
    // No behaviour change for anything that isn't a post-upload step.
    for (const [reason, message] of [
      [null, "write frame split: Broken pipe"],
      ["fs_write_failed_errno_28", "PS5 out of space"],
      [null, "no such file or directory"],
    ] as const) {
      expect(shouldAutoRecover(new Error(message), reason, message)).toBe(
        isAutoRecoverable(reason, message),
      );
    }
  });
});

describe("helper reasons", () => {
  it("recovers helper_not_ava1 only where the app can send the helper", () => {
    expect(isAutoRecoverable("helper_not_ava1", "")).toBe(true);
    expect(isAutoRecoverable("helper_not_ava1", "", { canSendHelper: true })).toBe(true);
    // The browser build has no payload_send: retrying cannot start a helper.
    expect(isAutoRecoverable("helper_not_ava1", "", { canSendHelper: false })).toBe(false);
    expect(
      shouldAutoRecover(new Error("x"), "helper_not_ava1", "", { canSendHelper: false }),
    ).toBe(false);
  });

  it.each(["helper_old", "ava1_failed", "helper_not_running"])(
    "never retries %s blindly (a person has to act, or it fails the same way)",
    (r) => {
      expect(isAutoRecoverable(r, "")).toBe(false);
    },
  );

  it("helper_starting is worth a retry", () => {
    expect(isAutoRecoverable("helper_starting", "")).toBe(true);
  });

  it("refineHelperReason turns helper_not_ava1 into helper_old once the console runs one", () => {
    expect(refineHelperReason("helper_not_ava1", "helper_old")).toBe("helper_old");
    expect(refineHelperReason("helper_not_ava1", "ava1_failed")).toBe("ava1_failed");
    expect(refineHelperReason("helper_not_ava1", "starting")).toBe("helper_starting");
    expect(refineHelperReason("helper_not_ava1", "not_running")).toBe("helper_not_ava1");
    expect(refineHelperReason("helper_not_ava1", null)).toBe("helper_not_ava1");
    expect(refineHelperReason("ava1_busy", "helper_old")).toBe("ava1_busy");
    expect(refineHelperReason(null, "helper_old")).toBeNull();
  });
});
