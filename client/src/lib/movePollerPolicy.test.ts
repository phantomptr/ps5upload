import { describe, expect, it } from "vitest";

import {
  CONSECUTIVE_FAILURE_LIMIT,
  classifyMovePollError,
  isExpectedNotInFlight,
} from "./movePollerPolicy";

describe("classifyMovePollError", () => {
  describe("known-current payload (>= 2.2.16)", () => {
    it("retries on the first transient error", () => {
      expect(
        classifyMovePollError("2.2.23", "tcp: connection reset", 1),
      ).toEqual({ kind: "retry" });
    });

    it("retries up to the failure limit", () => {
      expect(
        classifyMovePollError(
          "2.2.23",
          "engine HTTP 502: timeout",
          CONSECUTIVE_FAILURE_LIMIT - 1,
        ),
      ).toEqual({ kind: "retry" });
    });

    it("stops silently at the failure limit — no old-payload banner", () => {
      expect(
        classifyMovePollError(
          "2.2.23",
          "tcp: connection reset",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-silent" });
    });

    it("never blames the payload even when the error string matches a known old-payload signature", () => {
      // Regression: this was the original bug. The FS_OP_STATUS_ACK
      // body fix shipped in 2.2.16; on 2.2.23 the user should never
      // see "your payload is older than this app" no matter what
      // error text the engine produces.
      expect(
        classifyMovePollError(
          "2.2.23",
          "decode FS_OP_STATUS_ACK body: unexpected EOF",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-silent" });
      expect(
        classifyMovePollError(
          "2.2.23",
          "unsupported_frame",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-silent" });
    });

    it("treats a newer-than-app payload as current too", () => {
      expect(
        classifyMovePollError(
          "2.3.0-rc.1",
          "anything",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-silent" });
    });
  });

  describe("known-old payload", () => {
    it("latches the predates-2.2.7 banner immediately on a pre-FS_OP_STATUS payload", () => {
      expect(classifyMovePollError("2.2.5", "anything", 1)).toEqual({
        kind: "stop-old-payload",
        threshold: "2.2.7",
      });
    });

    it("latches the predates-2.2.16 banner immediately on a pre-buffer-fix payload", () => {
      expect(classifyMovePollError("2.2.10", "anything", 1)).toEqual({
        kind: "stop-old-payload",
        threshold: "2.2.16",
      });
      expect(classifyMovePollError("2.2.15", "anything", 1)).toEqual({
        kind: "stop-old-payload",
        threshold: "2.2.16",
      });
    });

    it("treats 2.2.7 itself as having FS_OP_STATUS but missing the buffer fix", () => {
      // The boundary case: 2.2.7 introduced the frame, 2.2.16 fixed
      // the body buffer overflow. Anything in [2.2.7, 2.2.16) gets
      // the predates-2.2.16 banner.
      expect(classifyMovePollError("2.2.7", "anything", 1)).toEqual({
        kind: "stop-old-payload",
        threshold: "2.2.16",
      });
    });

    it("does not latch on the exact buffer-fix version", () => {
      expect(
        classifyMovePollError(
          "2.2.16",
          "tcp: connection reset",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-silent" });
    });
  });

  describe("unknown payload version (probe didn't complete or pre-version build)", () => {
    it("retries before the failure limit even when the error matches an old-payload signature", () => {
      // A single decode-body error on a healthy payload is more
      // likely a transient parse race than an old payload. Don't
      // jump to conclusions until we've seen it consistently.
      expect(
        classifyMovePollError(null, "decode FS_OP_STATUS_ACK body", 1),
      ).toEqual({ kind: "retry" });
      expect(
        classifyMovePollError(
          null,
          "decode FS_OP_STATUS_ACK body",
          CONSECUTIVE_FAILURE_LIMIT - 1,
        ),
      ).toEqual({ kind: "retry" });
    });

    it("latches predates-2.2.16 after the limit on the buffer-overflow signature", () => {
      expect(
        classifyMovePollError(
          null,
          "decode FS_OP_STATUS_ACK body",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-old-payload", threshold: "2.2.16" });
    });

    it("latches predates-2.2.7 after the limit on the unsupported-frame signature", () => {
      expect(
        classifyMovePollError(
          null,
          "unsupported_frame",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-old-payload", threshold: "2.2.7" });
    });

    it("stops silently after the limit on a generic error", () => {
      // No version, no diagnostic signature → can't honestly say
      // "your payload is old." Just stop polling.
      expect(
        classifyMovePollError(null, "ECONNRESET", CONSECUTIVE_FAILURE_LIMIT),
      ).toEqual({ kind: "stop-silent" });
    });
  });

  describe("malformed payload version", () => {
    it("treats unparseable versions as unknown rather than guessing", () => {
      // compareVersions returns null on malformed input. A defensive
      // check: don't fall through to "looks current" — fall through
      // to the unknown-version branch so we still need the
      // consecutive-failure threshold to do anything user-visible.
      expect(classifyMovePollError("garbage", "anything", 1)).toEqual({
        kind: "retry",
      });
      expect(
        classifyMovePollError(
          "garbage",
          "decode FS_OP_STATUS_ACK body",
          CONSECUTIVE_FAILURE_LIMIT,
        ),
      ).toEqual({ kind: "stop-old-payload", threshold: "2.2.16" });
    });
  });
});

describe("isExpectedNotInFlight", () => {
  it("recognizes the engine's 404 surface", () => {
    expect(isExpectedNotInFlight("engine HTTP 404: op_id not in flight")).toBe(
      true,
    );
    expect(
      isExpectedNotInFlight("engine HTTP 404: something unrelated"),
    ).toBe(true);
    expect(
      isExpectedNotInFlight("ps5_fs_op_status: op_id 12345 not in flight"),
    ).toBe(true);
  });

  it("does not match real failures", () => {
    expect(isExpectedNotInFlight("decode FS_OP_STATUS_ACK body")).toBe(false);
    expect(isExpectedNotInFlight("unsupported_frame")).toBe(false);
    expect(isExpectedNotInFlight("ECONNRESET")).toBe(false);
    expect(isExpectedNotInFlight("engine HTTP 502: timeout")).toBe(false);
  });
});
