import { describe, expect, it, vi } from "vitest";

vi.mock("../../../state/lang", () => ({ useTr: () => (k: string) => k }));

import { listeningAfterRemove, macrosWithKey } from "./ControlsTab";

describe("macrosWithKey", () => {
  const macros = [
    { key: "KeyQ", steps: [{ buttons: 1, ms: 100 }] },
    { key: "", steps: [] },
  ];
  it("gives the waiting row the key pressed and leaves the others", () => {
    expect(macrosWithKey(macros, 1, "KeyE")).toEqual([macros[0], { key: "KeyE", steps: [] }]);
  });
  it("changes nothing on Esc, or for a row that is gone", () => {
    expect(macrosWithKey(macros, 1, "Escape")).toBeNull();
    expect(macrosWithKey(macros, 5, "KeyE")).toBeNull();
  });
});

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
