import { beforeEach, describe, expect, it } from "vitest";
import {
  consoleStayedSince,
  noteConsoleGone,
  noteHelperUp,
  resetLoaderSettle,
} from "./loaderSettle";

describe("whether a console stayed up since a moment", () => {
  beforeEach(() => resetLoaderSettle());

  it("is unknown for a console not watched then", () => {
    expect(consoleStayedSince("10.0.0.2", 100)).toBeNull();
    noteHelperUp("10.0.0.2", 500);
    expect(consoleStayedSince("10.0.0.2", 100)).toBeNull();
  });

  it("is true while it never went away, false once it did", () => {
    noteHelperUp("10.0.0.2", 100);
    expect(consoleStayedSince("10.0.0.2:9113", 200)).toBe(true);
    noteConsoleGone("10.0.0.2", 300);
    expect(consoleStayedSince("10.0.0.2", 200)).toBe(false);
    // A send after it came back: watched since.
    noteHelperUp("10.0.0.2", 400);
    expect(consoleStayedSince("10.0.0.2", 450)).toBe(true);
  });
});
