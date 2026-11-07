import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { DEFAULT_LOG_LEVEL, useDiagSettingsStore } from "../state/diagSettings";
import { installWindowStub } from "../test-utils/storage";
import { changeLevels, parseLevels, raiseAllForReproducing, refreshLevels } from "./logLevels";

type Call = { url: string; method: string; body: unknown };

function engine(calls: Call[], answer: (body: unknown) => { status: number; json: unknown }): typeof fetch {
  return (async (url: string, init?: RequestInit) => {
    const body = init?.body ? JSON.parse(String(init.body)) : null;
    calls.push({ url: String(url), method: init?.method ?? "GET", body });
    const a = answer(body);
    return { ok: a.status < 300, status: a.status, json: async () => a.json } as Response;
  }) as unknown as typeof fetch;
}

const LEVELS = {
  level: "info",
  categories: { rp: "trace" },
  until_ms: 1_800_000_060_000,
  raised: true,
  effective: { rp: "trace", install: "info" },
};

describe("logLevels", () => {
  beforeEach(() => {
    installWindowStub();
    useDiagSettingsStore.setState({ logLevel: DEFAULT_LOG_LEVEL, engine: null });
  });
  afterEach(() => {
    delete (globalThis as { window?: unknown }).window;
  });

  it("parses the engine's answer and drops what isn't a level", () => {
    expect(parseLevels({ level: "loud" })).toBeNull();
    const p = parseLevels({ ...LEVELS, categories: { rp: "trace", x: "nope" } });
    expect(p).toMatchObject({ level: "info", categories: { rp: "trace" }, until_ms: LEVELS.until_ms });
  });

  it("reads the engine's setting into the store", async () => {
    const calls: Call[] = [];
    expect(await refreshLevels(engine(calls, () => ({ status: 200, json: LEVELS })))).toBe(true);
    expect(calls[0].url).toMatch(/\/api\/logs\/level$/);
    expect(useDiagSettingsStore.getState().engine?.categories.rp).toBe("trace");
  });

  it("an engine from before levels leaves the local fallback", async () => {
    expect(await refreshLevels(engine([], () => ({ status: 404, json: { error: "no route" } })))).toBe(false);
    expect(useDiagSettingsStore.getState().engine).toBeNull();
  });

  it("the reproduce button raises everything to debug for half an hour", async () => {
    const calls: Call[] = [];
    await raiseAllForReproducing(
      30,
      engine(calls, () => ({ status: 200, json: { ...LEVELS, level: "debug", categories: {} } })),
    );
    expect(calls[0]).toMatchObject({ method: "PUT", body: { level: "debug", categories: {}, minutes: 30 } });
    expect(useDiagSettingsStore.getState().engine?.level).toBe("debug");
    // The local fallback never keeps a raise.
    expect(useDiagSettingsStore.getState().logLevel).toBe("info");
  });

  it("a refusal says why", async () => {
    await expect(
      changeLevels({ level: "info" }, engine([], () => ({ status: 400, json: { error: "unknown category" } }))),
    ).rejects.toThrow("unknown category");
  });
});
