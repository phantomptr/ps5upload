import { describe, expect, it } from "vitest";

import { rowOrderKey } from "./Table";

describe("what makes a table go back to its first page", () => {
  const key = (r: { id: string }) => r.id;

  it("is not a rebuilt array with the same rows in it", () => {
    const a = [{ id: "A", played: 1 }, { id: "B", played: 2 }];
    const again = [{ id: "A", played: 11 }, { id: "B", played: 2 }];
    expect(rowOrderKey(a, key)).toBe(rowOrderKey(again, key));
  });

  it("is other rows, or the same rows in another order", () => {
    const a = [{ id: "A" }, { id: "B" }];
    expect(rowOrderKey(a, key)).not.toBe(rowOrderKey([{ id: "B" }, { id: "A" }], key));
    expect(rowOrderKey(a, key)).not.toBe(rowOrderKey([{ id: "A" }], key));
  });
});
