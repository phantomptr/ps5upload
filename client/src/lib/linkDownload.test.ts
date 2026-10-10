import { describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://127.0.0.1:1" }));

import { canResumeExisting, startLinkDownload } from "./linkDownload";

const reply = (status: number, body: unknown) =>
  vi.fn(async () => new Response(JSON.stringify(body), { status }));

describe("startLinkDownload", () => {
  it("returns the started download", async () => {
    const f = reply(200, { download_id: "d1", path: "/dl/a.pkg", total: 10 });
    await expect(startLinkDownload({ url: "https://x/a.pkg", insecureTls: false }, f)).resolves.toEqual({
      kind: "started",
      id: "d1",
      path: "/dl/a.pkg",
      total: 10,
    });
  });

  it("attaches to the download already writing the same file", async () => {
    const f = reply(409, { error: "/dl/a.pkg is already being downloaded.", download_id: "first" });
    await expect(startLinkDownload({ url: "https://x/a.pkg", insecureTls: false }, f)).resolves.toEqual({
      kind: "attached",
      id: "first",
    });
  });

  it("describes a different file under the name instead of failing", async () => {
    const f = reply(409, {
      error: "exists",
      existing_path: "/dl/a.pkg",
      existing_bytes: 4,
      total: 10,
    });
    const r = await startLinkDownload({ url: "https://x/a.pkg", insecureTls: false }, f);
    expect(r).toMatchObject({ kind: "exists", path: "/dl/a.pkg", existingBytes: 4, total: 10 });
  });

  it("sends the user's choice for that file", async () => {
    const f = reply(200, { download_id: "d2", path: "/dl/a.pkg", total: 10 });
    await startLinkDownload({ url: "https://x/a.pkg", insecureTls: false, existing: "replace" }, f);
    const init = (f.mock.calls[0] as unknown as [string, RequestInit])[1];
    expect(JSON.parse(init.body as string)).toMatchObject({ existing: "replace" });
  });

  it("throws the engine's message for other errors", async () => {
    const f = reply(400, { error: "url is required" });
    await expect(startLinkDownload({ url: "", insecureTls: false }, f)).rejects.toThrow("url is required");
  });
});

describe("canResumeExisting", () => {
  it("offers Resume only for a file shorter than the link", () => {
    expect(canResumeExisting(4, 10)).toBe(true);
    expect(canResumeExisting(12, 10)).toBe(false);
    expect(canResumeExisting(0, 10)).toBe(false);
  });
});
