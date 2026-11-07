import { afterEach, describe, expect, it, vi } from "vitest";

import { ENGINE_TIMEOUT_MS, engineFetch, engineLongPost } from "./engineFetch";

describe("engineFetch", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("gives every request a limit, and leaves the rest of the request alone", async () => {
    const f = vi.fn(async (_url: string, _init?: RequestInit) => new Response("{}"));
    vi.stubGlobal("fetch", f);
    await engineFetch("http://e/x", { method: "POST", timeoutMs: 5 });
    const init = f.mock.calls[0][1]!;
    expect(init.method).toBe("POST");
    expect(init.signal).toBeInstanceOf(AbortSignal);
    expect("timeoutMs" in init).toBe(false);
  });

  it("says the engine did not answer when the limit runs out", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new DOMException("timed out", "TimeoutError");
    });
    await expect(engineFetch("http://e/x")).rejects.toThrow(
      `did not answer within ${ENGINE_TIMEOUT_MS / 1000} seconds`,
    );
  });

  it("leaves other failures as they are", async () => {
    vi.stubGlobal("fetch", async () => {
      throw new TypeError("Failed to fetch");
    });
    await expect(engineFetch("http://e/x")).rejects.toThrow("Failed to fetch");
  });

  it("does not set a limit over a caller's own signal", async () => {
    const f = vi.fn(async (_url: string, _init?: RequestInit) => new Response("{}"));
    vi.stubGlobal("fetch", f);
    const signal = new AbortController().signal;
    await engineFetch("http://e/x", { signal });
    expect(f.mock.calls[0][1]!.signal).toBe(signal);
  });
});

describe("engineLongPost (#712)", () => {
  afterEach(() => vi.unstubAllGlobals());

  const json = (v: unknown, status = 200) =>
    new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });

  it("asks the engine to run it in the background and follows it past any one request's limit", async () => {
    let reads = 0;
    const f = vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === "POST") return json({ op_id: "abc" }, 202);
      expect(url).toBe("http://e/api/ops/abc");
      reads++;
      if (reads < 4) return json({ id: "abc", status: "running", http_status: null });
      return json({ id: "abc", status: "done", http_status: 200, body: { ok: true, files: 4123 } });
    });
    vi.stubGlobal("fetch", f);
    const out = await engineLongPost("http://e", "/api/ps5/fs/extract", { op_id: 7 }, { pollMs: 1 });
    expect(out).toEqual({ status: 200, ok: true, body: { ok: true, files: 4123 } });
    const post = f.mock.calls[0][1]!;
    expect(new Headers(post.headers).get("prefer")).toBe("respond-async");
    expect(JSON.parse(String(post.body))).toEqual({ op_id: 7 });
    expect(reads).toBe(4);
  });

  it("gives the route's own failure, as the route gave it", async () => {
    vi.stubGlobal("fetch", async (_url: string, init?: RequestInit) =>
      init?.method === "POST"
        ? json({ op_id: "x" }, 202)
        : json({ status: "done", http_status: 409, body: { error: "cancelled", code: "conflict" } }),
    );
    const out = await engineLongPost("http://e", "/api/ps5/fs/copy", {}, { pollMs: 1 });
    expect(out).toEqual({ status: 409, ok: false, body: { error: "cancelled", code: "conflict" } });
  });

  it("takes an engine that answers the call itself at its word", async () => {
    vi.stubGlobal("fetch", async () => json({ ok: true }));
    expect(await engineLongPost("http://e", "/api/ps5/fs/delete", {})).toEqual({
      status: 200,
      ok: true,
      body: { ok: true },
    });
  });

  it("rides out a read that fails, and gives up only once the engine stays silent", async () => {
    let reads = 0;
    vi.stubGlobal("fetch", async (_url: string, init?: RequestInit) => {
      if (init?.method === "POST") return json({ op_id: "x" }, 202);
      reads++;
      if (reads === 2) throw new TypeError("Failed to fetch");
      if (reads < 4) return json({ status: "running" });
      return json({ status: "done", http_status: 200, body: { ok: true } });
    });
    expect((await engineLongPost("http://e", "/p", {}, { pollMs: 1, silentMs: 60_000 })).ok).toBe(true);

    vi.stubGlobal("fetch", async (_url: string, init?: RequestInit) => {
      if (init?.method === "POST") return json({ op_id: "x" }, 202);
      throw new TypeError("Failed to fetch");
    });
    await expect(engineLongPost("http://e", "/p", {}, { pollMs: 1, silentMs: 20 })).rejects.toThrow(
      "did not answer",
    );
  });

  it("says so when the engine has forgotten the operation", async () => {
    vi.stubGlobal("fetch", async (_url: string, init?: RequestInit) =>
      init?.method === "POST" ? json({ op_id: "x" }, 202) : json({ error: "no such operation" }, 404),
    );
    await expect(engineLongPost("http://e", "/p", {}, { pollMs: 1 })).rejects.toThrow("engine restarted");
  });
});
