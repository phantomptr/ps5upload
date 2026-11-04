import { describe, expect, it, vi } from "vitest";

vi.mock("../../../state/lang", () => ({ useTr: () => (k: string) => k }));

import { listeningAfterRemove } from "./ControlsTab";

describe("listeningAfterRemove", () => {
  it("stays off when nothing was waiting", () => {
    expect(listeningAfterRemove(null, 0)).toBeNull();
  });
  it("ends the wait of the removed row", () => {
    expect(listeningAfterRemove(1, 1)).toBeNull();
  });
  it("follows its row when an earlier one goes", () => {
    expect(listeningAfterRemove(2, 0)).toBe(1);
  });
  it("is unmoved when a later row goes", () => {
    expect(listeningAfterRemove(0, 2)).toBe(0);
  });
});
