import { describe, expect, it } from "vitest";
import { streamErrorMessage } from "./StreamPanel";
import { streamErrorFor } from "./streamErrors";

describe("streamErrorFor", () => {
  const lost = { code: "lost", detail: "" } as const;

  it("shows a stream's error on the page of the console it streamed", () => {
    expect(streamErrorFor(lost, "192.0.2.10", "192.0.2.10")).toBe(lost);
  });

  it("keeps it off another console's page", () => {
    expect(streamErrorFor(lost, "192.0.2.10", "192.0.2.11")).toBeNull();
  });

  it("shows it when the console is not known on either side", () => {
    expect(streamErrorFor(lost, null, "192.0.2.11")).toBe(lost);
    expect(streamErrorFor(lost, "192.0.2.10", null)).toBe(lost);
    expect(streamErrorFor(null, "192.0.2.10", "192.0.2.10")).toBeNull();
  });
});

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
    expect(m).toContain("Restart the PS5");
  });

  it("says a passcode start was cancelled or timed out, not that it cannot be done", () => {
    const cancelled = streamErrorMessage(tr, { code: "passcode_cancelled", detail: "" });
    const timeout = streamErrorMessage(tr, { code: "passcode_timeout", detail: "" });
    expect(cancelled).toContain("passcode");
    expect(timeout).toContain("Press Start");
    expect(cancelled).not.toBe(timeout);
    expect(`${cancelled} ${timeout}`).not.toContain("cannot enter it");
  });

  it("says why a session the PS5 took never started, not just that it stopped (#720)", () => {
    const m = streamErrorMessage(tr, { code: "no_session_id", detail: "the console never sent a session id over ctrl" });
    expect(m).toContain("another PS5 user");
    expect(m).toContain("press Start");
    expect(m).not.toBe(streamErrorMessage(tr, { code: "other", detail: "" }));
  });

  it("after five wrong passcodes says to enter it on the PS5", () => {
    expect(streamErrorMessage(tr, { code: "passcode_too_many", detail: "" })).toBe(
      "Too many wrong passcodes; enter it on the PS5.",
    );
  });
});

describe("a session the PS5 handed to something else", () => {
  it("says it was taken over, not Sony's server words (#769)", () => {
    const m = streamErrorMessage(tr, { code: "console_ended", detail: "Server shutting down" });
    expect(m).toMatch(/took it over/);
    expect(m).not.toMatch(/Server shutting down/);
  });

  it("names the Play Together player who picked the stream's PS5 user on the TV", () => {
    const m = streamErrorMessage(tr, { code: "console_ended", detail: "Server shutting down", guestPicked: 3 });
    expect(m).toBe("Player 3 picked your PS5 user on the TV, so the PS5 ended your stream. Press Start to play again.");
  });

  it("passes any other reason through as the PS5 gave it", () => {
    const m = streamErrorMessage(tr, { code: "console_ended", detail: "Remote Play turned off" });
    expect(m).toBe("The PS5 ended the stream: Remote Play turned off");
  });
});
