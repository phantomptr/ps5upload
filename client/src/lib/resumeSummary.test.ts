import { describe, expect, it } from "vitest";
import { resumeSummary } from "./resumeSummary";

describe("resumeSummary", () => {
  it("is null for a fresh upload (nothing already on the console)", () => {
    expect(resumeSummary(0, 10, 0, 1000)).toBeNull();
  });

  it("counts the files already present and what is left to send", () => {
    expect(resumeSummary(4, 10, 600, 1000)).toEqual({
      done: 4,
      total: 10,
      haveBytes: 600,
      sendBytes: 400,
    });
  });

  it("never reports more present than the whole, nor a negative remainder", () => {
    expect(resumeSummary(12, 10, 2000, 1000)).toEqual({
      done: 10,
      total: 10,
      haveBytes: 1000,
      sendBytes: 0,
    });
  });

  it("falls back to the skipped count when the planned list is not known yet", () => {
    expect(resumeSummary(3, 0, 300, 0)).toEqual({
      done: 3,
      total: 3,
      haveBytes: 300,
      sendBytes: 0,
    });
  });
});
