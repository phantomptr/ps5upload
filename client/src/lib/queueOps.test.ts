import { describe, expect, it } from "vitest";
import {
  dropSide,
  moveItemDownWithinGroup,
  moveItemToWithinGroup,
  moveItemUpWithinGroup,
  nextPending,
  patchItem,
  removeItem,
  resetFailedToPending,
  resetRunningToPending,
} from "./queueOps";

type Item = { id: string; status: "pending" | "running" | "done" | "failed" };

const items = (): Item[] => [
  { id: "a", status: "done" },
  { id: "b", status: "pending" },
  { id: "c", status: "pending" },
];

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

type RunItem = Item & {
  bytesSent: number;
  totalBytes: number;
  bytesPerSec: number;
};

const runItem = (
  id: string,
  status: RunItem["status"],
  bytesSent = 100,
  totalBytes = 200,
  bytesPerSec = 50,
): RunItem => ({ id, status, bytesSent, totalBytes, bytesPerSec });

describe("resetRunningToPending", () => {
  it("flips running entries to pending and zeros live counters", () => {
    const arr: RunItem[] = [
      runItem("a", "done", 1024, 1024, 0),
      runItem("b", "running", 600, 1024, 12345),
      runItem("c", "pending", 0, 0, 0),
    ];
    const next = resetRunningToPending(arr);
    expect(next.map((i) => i.status)).toEqual(["done", "pending", "pending"]);
    expect(next[1].bytesSent).toBe(0);
    expect(next[1].totalBytes).toBe(0);
    expect(next[1].bytesPerSec).toBe(0);
  });

  it("marks a stopped running item interrupted, so it offers Resume", () => {
    const next = resetRunningToPending([runItem("b", "running", 600, 1024, 1)]);
    expect((next[0] as RunItem & { interrupted?: boolean }).interrupted).toBe(true);
  });

  it("leaves done items untouched (counters preserved)", () => {
    const arr: RunItem[] = [runItem("a", "done", 1024, 1024, 0)];
    const next = resetRunningToPending(arr);
    expect(next).toBe(arr);
  });

  it("clears transient auto-recovery flags on a stopped running item", () => {
    // A running item caught mid-recovery must not be persisted as a pending
    // row that still claims to be recovering.
    const arr = [
      {
        ...runItem("b", "running", 600, 1024, 12345),
        recovering: true,
        recoverAttempt: 2,
      },
    ];
    const next = resetRunningToPending(arr);
    expect(next[0].status).toBe("pending");
    expect(next[0].recovering).toBe(false);
    expect(next[0].recoverAttempt).toBe(0);
  });

  it("returns same reference when nothing was running", () => {
    const arr: RunItem[] = [
      runItem("a", "pending", 0, 0, 0),
      runItem("b", "failed", 100, 200, 0),
    ];
    expect(resetRunningToPending(arr)).toBe(arr);
  });

  it("preserves non-running items by reference (only running entries are rebuilt)", () => {
    const done = runItem("a", "done", 1024, 1024, 0);
    const running = runItem("b", "running", 600, 1024, 12345);
    const arr: RunItem[] = [done, running];
    const next = resetRunningToPending(arr);
    expect(next[0]).toBe(done);
    expect(next[1]).not.toBe(running);
  });

  it("only resets running items matching the predicate (per-console stop)", () => {
    type HostItem = RunItem & { addr: string };
    const a = { ...runItem("a", "running", 1, 2, 3), addr: "10.0.0.1" };
    const b = { ...runItem("b", "running", 1, 2, 3), addr: "10.0.0.2" };
    const next = resetRunningToPending<HostItem>(
      [a, b],
      (it) => it.addr === "10.0.0.1",
    );
    expect(next[0].status).toBe("pending"); // host .1 reset
    expect(next[1].status).toBe("running"); // host .2 untouched
    expect(next[1]).toBe(b);
  });
});

// ── Group-aware reorder (per-console queue) ──────────────────────────────────

describe("moveItemUpWithinGroup / moveItemDownWithinGroup", () => {
  type G = { id: string; host: string };
  const groupOf = (it: G) => it.host;
  // Interleaved across two consoles: A B A B A
  const arr: G[] = [
    { id: "a1", host: "A" },
    { id: "b1", host: "B" },
    { id: "a2", host: "A" },
    { id: "b2", host: "B" },
    { id: "a3", host: "A" },
  ];

  it("up swaps with the nearest EARLIER same-group item, skipping other consoles", () => {
    // a2 (index 2) up → swaps with a1 (index 0), leaving b1 at index 1.
    const next = moveItemUpWithinGroup(arr, "a2", groupOf);
    expect(next.map((i) => i.id)).toEqual(["a2", "b1", "a1", "b2", "a3"]);
    // B's relative order is untouched.
    expect(next.filter((i) => i.host === "B").map((i) => i.id)).toEqual([
      "b1",
      "b2",
    ]);
  });

  it("down swaps with the nearest LATER same-group item", () => {
    // a2 (index 2) down → swaps with a3 (index 4).
    const next = moveItemDownWithinGroup(arr, "a2", groupOf);
    expect(next.map((i) => i.id)).toEqual(["a1", "b1", "a3", "b2", "a2"]);
  });

  it("returns same reference when item is first/last in its group", () => {
    expect(moveItemUpWithinGroup(arr, "a1", groupOf)).toBe(arr); // a1 first A
    expect(moveItemDownWithinGroup(arr, "a3", groupOf)).toBe(arr); // a3 last A
    expect(moveItemUpWithinGroup(arr, "b1", groupOf)).toBe(arr); // b1 first B
    expect(moveItemDownWithinGroup(arr, "b2", groupOf)).toBe(arr); // b2 last B
  });

  it("returns same reference when id is absent", () => {
    expect(moveItemUpWithinGroup(arr, "nope", groupOf)).toBe(arr);
    expect(moveItemDownWithinGroup(arr, "nope", groupOf)).toBe(arr);
  });
});

describe("moveItemToWithinGroup (drag and drop)", () => {
  type G = { id: string; host: string };
  const groupOf = (it: G) => it.host;
  // A B A B A A
  const arr: G[] = [
    { id: "a1", host: "A" },
    { id: "b1", host: "B" },
    { id: "a2", host: "A" },
    { id: "b2", host: "B" },
    { id: "a3", host: "A" },
    { id: "a4", host: "A" },
  ];
  const ids = (xs: G[]) => xs.map((x) => x.id);

  it("moves a row down past several of its group, keeping other groups in place", () => {
    const next = moveItemToWithinGroup(arr, "a1", "a3", groupOf);
    expect(ids(next)).toEqual(["a2", "b1", "a3", "b2", "a1", "a4"]);
    // B rows keep their slots.
    expect(next[1].id).toBe("b1");
    expect(next[3].id).toBe("b2");
  });

  it("moves a row up to the top of its group", () => {
    const next = moveItemToWithinGroup(arr, "a4", "a1", groupOf);
    expect(ids(next)).toEqual(["a4", "b1", "a1", "b2", "a2", "a3"]);
  });

  it("dropping on a neighbour is the same as one arrow press", () => {
    expect(ids(moveItemToWithinGroup(arr, "a2", "a3", groupOf))).toEqual(
      ids(moveItemDownWithinGroup(arr, "a2", groupOf)),
    );
    expect(ids(moveItemToWithinGroup(arr, "a2", "a1", groupOf))).toEqual(
      ids(moveItemUpWithinGroup(arr, "a2", groupOf)),
    );
  });

  it("ignores a drop on another group, on itself, or on a missing row", () => {
    expect(moveItemToWithinGroup(arr, "a1", "b2", groupOf)).toBe(arr);
    expect(moveItemToWithinGroup(arr, "a1", "a1", groupOf)).toBe(arr);
    expect(moveItemToWithinGroup(arr, "nope", "a1", groupOf)).toBe(arr);
    expect(moveItemToWithinGroup(arr, "a1", "nope", groupOf)).toBe(arr);
  });

  it("draws the drop line on the side the row is moving to", () => {
    const order = ["x", "y", "z"];
    expect(dropSide(order, "x", "z")).toBe("after");
    expect(dropSide(order, "z", "x")).toBe("before");
    expect(dropSide(order, "y", "y")).toBeNull();
    expect(dropSide(order, "y", "q")).toBeNull();
  });
});
