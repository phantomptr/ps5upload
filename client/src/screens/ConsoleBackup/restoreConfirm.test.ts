import { describe, expect, it, vi } from "vitest";

import { restoreAfterConfirm, restoreConfirmOptions } from "./restoreConfirm";

const tr = ((_k: string, _v: unknown, fallback?: string) => fallback ?? _k) as Parameters<
  typeof restoreConfirmOptions
>[0];

describe("restoring a console backup", () => {
  it("asks first, as a destructive action that says files are replaced", () => {
    const o = restoreConfirmOptions(tr);
    expect(o.destructive).toBe(true);
    expect(o.title).toBe("Restore to this PS5");
    expect(o.message).toContain("replaced");
    expect(o.confirmLabel).toBe("Restore");
  });

  it("does nothing when the user says no", async () => {
    const run = vi.fn(async () => {});
    expect(await restoreAfterConfirm(async () => false, restoreConfirmOptions(tr), run)).toBe(false);
    expect(run).not.toHaveBeenCalled();
  });

  it("restores once the user says yes", async () => {
    const run = vi.fn(async () => {});
    const confirm = vi.fn(async () => true);
    expect(await restoreAfterConfirm(confirm, restoreConfirmOptions(tr), run)).toBe(true);
    expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ destructive: true }));
    expect(run).toHaveBeenCalledTimes(1);
  });
});
