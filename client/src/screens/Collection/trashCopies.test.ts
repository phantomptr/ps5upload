import { beforeEach, describe, expect, it, vi } from "vitest";

const { trashPreview, trashApply, pushNotification } = vi.hoisted(() => ({
  trashPreview: vi.fn(),
  trashApply: vi.fn(),
  pushNotification: vi.fn(),
}));

vi.mock("../../api/collection", async (original) => ({
  ...(await original<typeof import("../../api/collection")>()),
  collection: { trashPreview, trashApply },
}));
vi.mock("../../state/collection", () => ({ loadCollection: vi.fn() }));
vi.mock("../../state/notifications", () => ({ pushNotification }));

import { CollectionApiError } from "../../api/collection";
import { trashCopies } from "./trashCopies";

const tr = (_k: string, vars: Record<string, unknown> | undefined, fallback?: string) =>
  Object.entries(vars ?? {}).reduce((t, [k, v]) => t.replace(`{${k}}`, String(v)), fallback ?? "");

const item = { path: "/g/G_0.pkg", name: "G_0.pkg", game_id: "CUSA00001", title: "G", size_bytes: 150 };

describe("trashCopies", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    trashApply.mockResolvedValue({ moved: [item.path], failed: [] });
  });

  it("says every file of a split copy goes with it", async () => {
    trashPreview.mockResolvedValue({
      token: "t",
      items: [{ ...item, parts: ["/g/G_1.pkg"] }],
      total_bytes: 150,
      available: true,
      mode: "trash",
    });
    const confirm = vi.fn().mockResolvedValue(true);
    await trashCopies([item.path], confirm, tr as never);
    expect(confirm.mock.calls[0][0].message).toContain("Every file of a split copy goes with it.");
    expect(trashApply).toHaveBeenCalledWith("t");
  });

  it("does not mention parts for a copy that is one file", async () => {
    trashPreview.mockResolvedValue({
      token: "t",
      items: [item],
      total_bytes: 150,
      available: true,
      mode: "trash",
    });
    const confirm = vi.fn().mockResolvedValue(false);
    await trashCopies([item.path], confirm, tr as never);
    expect(confirm.mock.calls[0][0].message).not.toContain("split");
    expect(trashApply).not.toHaveBeenCalled();
  });

  it("words a copy that changed since the scan in the user's language", async () => {
    trashPreview.mockRejectedValue(new CollectionApiError("x.pkg changed; scan again", "changed"));
    const confirm = vi.fn();
    await trashCopies([item.path], confirm, tr as never);
    expect(confirm).not.toHaveBeenCalled();
    expect(pushNotification).toHaveBeenCalledWith(
      "error",
      "A copy changed since the last scan. Scan again, then try again.",
    );
  });
});
