import { describe, expect, it } from "vitest";

import { loadTitled, resultFor, type TitledResult } from "./gameViewState";

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

describe("game page view state", () => {
  it("drops a late reply for the game the page already left", async () => {
    let onScreen = "PPSA00001";
    let held: TitledResult<string> | null = null;
    const apply = (r: TitledResult<string>) => (held = r);
    const a = deferred<string>();
    const b = deferred<string>();
    const reads: Record<string, Promise<string>> = { PPSA00001: a.promise, PPSA00002: b.promise };

    const first = loadTitled("PPSA00001", (id) => reads[id], () => onScreen, apply);
    onScreen = "PPSA00002";
    const second = loadTitled("PPSA00002", (id) => reads[id], () => onScreen, apply);
    b.resolve("Game B");
    await second;
    a.resolve("Game A"); // the old page's reply lands last
    await first;

    expect(held).toEqual({ titleId: "PPSA00002", value: "Game B", error: null });
  });

  it("never shows a held result under another title", () => {
    const held = { titleId: "PPSA00001", value: "Game A", error: null };
    expect(resultFor(held, "PPSA00001")?.value).toBe("Game A");
    expect(resultFor(held, "PPSA00002")).toBeNull();
    expect(resultFor(null, "PPSA00002")).toBeNull();
  });

  it("keeps the error with the title it belongs to", async () => {
    let held: TitledResult<string> | null = null;
    await loadTitled(
      "PPSA00001",
      async () => {
        throw new Error("engine away");
      },
      () => "PPSA00001",
      (r) => (held = r),
    );
    expect(held).toEqual({ titleId: "PPSA00001", value: null, error: "engine away" });
  });
});
