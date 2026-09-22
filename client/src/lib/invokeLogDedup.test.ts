import { beforeEach, describe, expect, it } from "vitest";
import {
  observeInvokeOutcome,
  resetInvokeLogDedup,
  REMINDER_MS,
} from "./invokeLogDedup";

describe("command failure log dedup", () => {
  beforeEach(() => resetInvokeLogDedup());

  /* The first failure always speaks — suppression must never hide the event
   * itself, only its echo. */
  it("reports the first failure", () => {
    expect(observeInvokeOutcome("process_list_get", "connect refused", 0)).toEqual(
      { kind: "warn", suppressed: 0 },
    );
  });

  /* The case from the field: 63 identical lines out of 100, for one outage. */
  it("stays quiet through a run of identical failures", () => {
    observeInvokeOutcome("process_list_get", "connect refused", 0);
    for (let t = 10_000; t < REMINDER_MS; t += 10_000) {
      expect(observeInvokeOutcome("process_list_get", "connect refused", t)).toEqual(
        { kind: "quiet" },
      );
    }
  });

  /* Quiet must not mean forgotten: a long outage still proves it is ongoing,
   * and says how much was swallowed. */
  it("reminds once the quiet window elapses, with the count", () => {
    observeInvokeOutcome("process_list_get", "connect refused", 0);
    observeInvokeOutcome("process_list_get", "connect refused", 10_000);
    observeInvokeOutcome("process_list_get", "connect refused", 20_000);
    expect(
      observeInvokeOutcome("process_list_get", "connect refused", REMINDER_MS),
    ).toEqual({ kind: "warn", suppressed: 3 });
  });

  /* A different error is different information — a console refusing for a new
   * reason is not the same outage. */
  it("reports a changed message immediately", () => {
    observeInvokeOutcome("process_list_get", "connect refused", 0);
    expect(observeInvokeOutcome("process_list_get", "timed out", 1_000)).toEqual({
      kind: "warn",
      suppressed: 0,
    });
  });

  /* Recovery is the line a reader actually wants, and it carries the size of
   * the outage. */
  it("announces recovery with the number swallowed", () => {
    observeInvokeOutcome("process_list_get", "connect refused", 0);
    observeInvokeOutcome("process_list_get", "connect refused", 10_000);
    observeInvokeOutcome("process_list_get", "connect refused", 20_000);
    expect(observeInvokeOutcome("process_list_get", undefined, 30_000)).toEqual({
      kind: "recovered",
      suppressed: 2,
    });
  });

  /* A success that follows no failure, or a single reported failure, must not
   * emit a pointless "recovered" line. */
  it("says nothing when there was nothing to recover from", () => {
    expect(observeInvokeOutcome("x", undefined, 0)).toEqual({ kind: "quiet" });
    observeInvokeOutcome("x", "boom", 1);
    expect(observeInvokeOutcome("x", undefined, 2)).toEqual({ kind: "quiet" });
  });

  /* Commands are tracked independently: one console down must not silence a
   * different command's first failure. */
  it("tracks each command separately", () => {
    observeInvokeOutcome("a", "boom", 0);
    expect(observeInvokeOutcome("b", "boom", 0)).toEqual({
      kind: "warn",
      suppressed: 0,
    });
  });
});
