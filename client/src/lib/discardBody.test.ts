import { describe, expect, it, vi } from "vitest";

import { discardBody } from "./discardBody";

describe("discardBody", () => {
  it("cancels an unread body", async () => {
    const res = new Response("not found", { status: 404 });
    const cancel = vi.spyOn(res.body!, "cancel");
    await discardBody(res);
    expect(cancel).toHaveBeenCalledTimes(1);
  });

  it("leaves a body that was already read", async () => {
    const res = new Response('{"a":1}', { status: 200 });
    await res.json();
    await expect(discardBody(res)).resolves.toBeUndefined();
  });

  it("leaves a body a reader holds", async () => {
    const res = new Response("streaming", { status: 200 });
    const reader = res.body!.getReader();
    const cancel = vi.spyOn(res.body!, "cancel");
    await discardBody(res);
    expect(cancel).not.toHaveBeenCalled();
    reader.releaseLock();
  });

  it("takes a response without a body, or none at all", async () => {
    await expect(discardBody(new Response(null, { status: 204 }))).resolves.toBeUndefined();
    await expect(discardBody(null)).resolves.toBeUndefined();
    await expect(discardBody(undefined)).resolves.toBeUndefined();
  });

  it("never throws when the cancel itself fails", async () => {
    const res = new Response("x", { status: 500 });
    vi.spyOn(res.body!, "cancel").mockRejectedValue(new Error("boom"));
    await expect(discardBody(res)).resolves.toBeUndefined();
  });
});
