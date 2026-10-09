import { describe, expect, it, vi } from "vitest";

const invoke = vi.fn();
vi.mock("../lib/invokeLogged", () => ({ invoke: (...a: unknown[]) => invoke(...a) }));

import { cachedCollectionCover, collectionCoverDataUrl } from "./collection";

describe("a Collection cover over the IPC", () => {
  it("comes back as a data URL and is kept for the session", async () => {
    invoke.mockResolvedValueOnce("data:image/png;base64,AAAA");
    expect(await collectionCoverDataUrl("PPSA11386")).toBe("data:image/png;base64,AAAA");
    expect(invoke).toHaveBeenCalledWith("collection_cover_data", { gameId: "PPSA11386" });
    expect(cachedCollectionCover("PPSA11386")).toBe("data:image/png;base64,AAAA");
    // Held now: no second trip.
    expect(await collectionCoverDataUrl("PPSA11386")).toBe("data:image/png;base64,AAAA");
    expect(invoke).toHaveBeenCalledTimes(1);
  });
  it("is null when the engine has none", async () => {
    invoke.mockRejectedValueOnce(new Error("engine HTTP 404"));
    expect(await collectionCoverDataUrl("PPSA00000")).toBeNull();
  });
});
