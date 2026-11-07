import { describe, expect, it, vi } from "vitest";

import { fetchReleaseNotes } from "./releaseNotes";

const json = (v: unknown, status = 200) =>
  new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });

describe("fetchReleaseNotes", () => {
  it("reads the release body of the version's tag", async () => {
    const f = vi.fn(async (_url: string, _init?: RequestInit) => json({ body: "Games:\r\n\r\n- New: zstd packs" }));
    expect(await fetchReleaseNotes("11.4.0", f as unknown as typeof fetch)).toBe("Games:\n\n- New: zstd packs");
    expect(f.mock.calls[0][0]).toBe("https://api.github.com/repos/phantomptr/ps5upload/releases/tags/v11.4.0");
    await fetchReleaseNotes("v11.4.0", f as unknown as typeof fetch);
    expect(f.mock.calls[1][0]).toMatch(/tags\/v11\.4\.0$/);
  });

  it("fails on an error answer or an empty body", async () => {
    await expect(fetchReleaseNotes("1.0.0", (async () => json({}, 403)) as unknown as typeof fetch)).rejects.toThrow(
      "403",
    );
    await expect(
      fetchReleaseNotes("1.0.0", (async () => json({ body: "  " })) as unknown as typeof fetch),
    ).rejects.toThrow("no notes");
  });
});
