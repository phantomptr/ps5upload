import { describe, expect, it } from "vitest";
import {
  moveItemDown,
  moveItemUp,
  nextPending,
  patchItem,
  removeItem,
  resetFailedToPending,
} from "./queueOps";

type Item = { id: string; status: "pending" | "running" | "done" | "failed" };

const items = (): Item[] => [
  { id: "a", status: "done" },
  { id: "b", status: "pending" },
  { id: "c", status: "pending" },
];

describe("moveItemUp", () => {
  it("swaps with the previous slot", () => {
    expect(moveItemUp(items(), "c").map((i) => i.id)).toEqual([
      "a",
      "c",
      "b",
    ]);
  });
  it("returns same reference when already at the top", () => {
    const arr = items();
    expect(moveItemUp(arr, "a")).toBe(arr);
  });
  it("returns same reference when id missing", () => {
    const arr = items();
    expect(moveItemUp(arr, "missing")).toBe(arr);
  });
});

describe("moveItemDown", () => {
  it("swaps with the next slot", () => {
    expect(moveItemDown(items(), "a").map((i) => i.id)).toEqual([
      "b",
      "a",
      "c",
    ]);
  });
  it("returns same reference when already at the bottom", () => {
    const arr = items();
    expect(moveItemDown(arr, "c")).toBe(arr);
  });
  it("returns same reference when id missing", () => {
    const arr = items();
    expect(moveItemDown(arr, "missing")).toBe(arr);
  });
});

describe("removeItem", () => {
  it("drops the matching id", () => {
    expect(removeItem(items(), "b").map((i) => i.id)).toEqual(["a", "c"]);
  });
  it("returns same reference when id missing", () => {
    const arr = items();
    expect(removeItem(arr, "missing")).toBe(arr);
  });
});

describe("patchItem", () => {
  it("merges the patch into the matching item", () => {
    const next = patchItem(items(), "b", { status: "running" });
    expect(next.find((i) => i.id === "b")?.status).toBe("running");
  });
  it("returns same reference when id missing", () => {
    const arr = items();
    expect(patchItem(arr, "missing", { status: "done" })).toBe(arr);
  });
});

describe("nextPending", () => {
  it("returns the first pending item in order", () => {
    expect(nextPending(items())?.id).toBe("b");
  });
  it("returns null when nothing is pending", () => {
    expect(
      nextPending([
        { id: "a", status: "done" },
        { id: "b", status: "failed" },
      ]),
    ).toBeNull();
  });
});

describe("resetFailedToPending", () => {
  it("flips only failed entries", () => {
    const arr: Item[] = [
      { id: "a", status: "done" },
      { id: "b", status: "failed" },
      { id: "c", status: "running" },
    ];
    const next = resetFailedToPending(arr);
    expect(next.map((i) => i.status)).toEqual(["done", "pending", "running"]);
  });
  it("returns same reference when nothing failed", () => {
    const arr: Item[] = [{ id: "a", status: "done" }];
    expect(resetFailedToPending(arr)).toBe(arr);
  });
});
