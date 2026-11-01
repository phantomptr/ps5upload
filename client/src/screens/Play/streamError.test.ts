import { describe, expect, it } from "vitest";
import { streamErrorMessage } from "./StreamPanel";

// The English fallback, as the app shows it without a translation.
const tr = ((_key: string, _vars: unknown, fallback?: string) => fallback ?? "") as Parameters<
  typeof streamErrorMessage
>[0];

describe("streamErrorMessage", () => {
  it("says a stream lost mid-way dropped, not just that it stopped", () => {
    const m = streamErrorMessage(tr, { code: "lost", detail: "" });
    expect(m).toContain("dropped during the stream");
    expect(m).not.toBe(streamErrorMessage(tr, { code: "other", detail: "" }));
  });

  it("tells how to turn Remote Play on when the console has it switched off", () => {
    const m = streamErrorMessage(tr, { code: "rp_off", detail: "" });
    expect(m).toContain("Settings › System › Remote Play");
  });

  it("says what to check when the PS5 refuses with a code we don't know", () => {
    const m = streamErrorMessage(tr, { code: "refused", detail: "session init refused (HTTP 403): 80108b26" });
    expect(m).toContain("refused to start Remote Play");
  });

  it("says a passcode start was cancelled or timed out, not that it cannot be done", () => {
    const cancelled = streamErrorMessage(tr, { code: "passcode_cancelled", detail: "" });
    const timeout = streamErrorMessage(tr, { code: "passcode_timeout", detail: "" });
    expect(cancelled).toContain("passcode");
    expect(timeout).toContain("Press Start");
    expect(cancelled).not.toBe(timeout);
    expect(`${cancelled} ${timeout}`).not.toContain("cannot enter it");
  });

  it("after five wrong passcodes says to enter it on the PS5", () => {
    expect(streamErrorMessage(tr, { code: "passcode_too_many", detail: "" })).toBe(
      "Too many wrong passcodes; enter it on the PS5.",
    );
  });
});
