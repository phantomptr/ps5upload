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
});
