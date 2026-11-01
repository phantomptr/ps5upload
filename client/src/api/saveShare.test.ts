import { afterEach, describe, expect, it, vi } from "vitest";
import { copySaveToUser, putSharedSave, SaveCopyError, shareSave } from "./saveShare";

afterEach(() => vi.unstubAllGlobals());

function stub(status: number, body: unknown) {
  const calls: Array<{ url: string; init: RequestInit }> = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
    }),
  );
  return calls;
}

describe("save sharing API", () => {
  it("shares and loads the whole save folder", async () => {
    let calls = stub(500, { error: "x" });
    await expect(shareSave("1.2.3.4", "/user/home/1/savedata_prospero/PPSA01650")).rejects.toThrow("x");
    expect(JSON.parse(calls[0].init.body as string)).toEqual({ addr: "1.2.3.4", folder: "/user/home/1/savedata_prospero/PPSA01650" });
    calls = stub(200, { ok: true, parts: ["sdimg_STARBOARD"] });
    expect(await putSharedSave("1.2.3.4", "/f", -1, new Blob(["z"]))).toEqual(["sdimg_STARBOARD"]);
    const q = new URL(calls[0].url).searchParams;
    expect([q.get("folder"), q.get("user_id")]).toEqual(["/f", "4294967295"]);
  });

  it("carries the parts the target is missing", async () => {
    stub(502, { error: "isn't on the other user yet", missing: ["sdimg_STARBOARD"] });
    const e = await copySaveToUser("1.2.3.4", { title_id: "PPSA01650", from_user: 1, to_user: 2 }).catch((x) => x);
    expect(e).toBeInstanceOf(SaveCopyError);
    expect((e as SaveCopyError).missing).toEqual(["sdimg_STARBOARD"]);
  });
});
