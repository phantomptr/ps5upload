import { afterEach, expect, it, vi } from "vitest";

import { fsCopy, fsDelete, fsExtract, fsMove } from "./ps5";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine" }));

afterEach(() => vi.unstubAllGlobals());

const json = (v: unknown, status = 200) =>
  new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });

/** An engine whose route runs for `reads` status reads, then answers `status` with `body`. */
function engine(reads: number, status: number, body: unknown) {
  const posts: Array<{ url: string; body: unknown; prefer: string | null }> = [];
  let seen = 0;
  vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
    if (init?.method === "POST") {
      posts.push({ url, body: JSON.parse(String(init.body)), prefer: new Headers(init.headers).get("prefer") });
      return json({ op_id: "op-1" }, 202);
    }
    seen++;
    return seen < reads
      ? json({ status: "running" })
      : json({ status: "done", http_status: status, body });
  });
  return posts;
}

// #712: a 23 GB archive of thousands of files extracted for longer than one request may wait, and
// the app said the engine did not answer while the console went on extracting.
it("follows an extract to its end instead of timing out the request", async () => {
  const posts = engine(5, 200, { ok: true, path: "/data/x", files: 4321, dirs: 9, bytes: 23e9 });
  const out = await fsExtract("192.168.1.5:9113", "/data/x.zip", "/data/x", 42);
  expect(out.files).toBe(4321);
  expect(posts).toEqual([
    {
      url: "http://engine/api/ps5/fs/extract",
      body: expect.objectContaining({ path: "/data/x.zip", dest: "/data/x", op_id: 42 }),
      prefer: "respond-async",
    },
  ]);
});

it("keeps the helper's cause and a Stop as the callers read them", async () => {
  engine(1, 502, { error: "fs_extract_password_required", code: "bad_gateway" });
  await expect(fsExtract("h:9113", "/a.7z", "/a", 1)).rejects.toThrow("fs_extract_password_required");
  engine(1, 409, { error: "cancelled", code: "conflict" });
  await expect(fsExtract("h:9113", "/a.7z", "/a", 1)).rejects.toThrow("cancelled");
});

it("runs copy, move and delete the same way", async () => {
  const posts = engine(2, 200, { ok: true });
  await fsCopy("h:9113", "/a", "/b", 3, true);
  await fsMove("h:9113", "/a", "/b", 4);
  await fsDelete("h:9113", "/a", 5);
  expect(posts.map((p) => [p.url, p.prefer])).toEqual([
    ["http://engine/api/ps5/fs/copy", "respond-async"],
    ["http://engine/api/ps5/fs/move", "respond-async"],
    ["http://engine/api/ps5/fs/delete", "respond-async"],
  ]);
  expect(posts[0].body).toMatchObject({ from: "/a", to: "/b", op_id: 3, overwrite: true });
  expect(posts[1].body).toMatchObject({ op_id: 4, overwrite: false });
  expect(posts[2].body).toMatchObject({ path: "/a", op_id: 5 });

  engine(1, 409, { error: "fs_copy_dest_exists", code: "conflict" });
  await expect(fsCopy("h:9113", "/a", "/b")).rejects.toThrow("fs_copy_dest_exists");
});
