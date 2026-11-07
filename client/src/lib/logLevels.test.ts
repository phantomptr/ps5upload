import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { DEFAULT_LOG_LEVEL, useDiagSettingsStore } from "../state/diagSettings";
import { installWindowStub } from "../test-utils/storage";
import { changeLevels, parseLevels, raiseAllForReproducing, refreshLevels, stopRaising } from "./logLevels";

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

  it("the reproduce button and its stop keep the categories someone quietened", async () => {
    useDiagSettingsStore.setState({
      engine: {
        level: "info",
        categories: { rp: "trace", install: "error", mesh: "warn" },
        until_ms: Date.now() + 60_000,
        raised: true,
        effective: {},
      },
    });
    const calls: Call[] = [];
    const answer = () => ({ status: 200, json: { ...LEVELS, level: "debug", categories: { install: "error", mesh: "warn" } } });
    await raiseAllForReproducing(30, engine(calls, answer));
    expect(calls[0].body).toEqual({ level: "debug", categories: { install: "error", mesh: "warn" }, minutes: 30 });
    // Stop: only the raise comes down.
    useDiagSettingsStore.setState({
      engine: {
        level: "debug",
        categories: { rp: "trace", install: "error" },
        until_ms: Date.now() + 60_000,
        raised: true,
        effective: {},
      },
    });
    await stopRaising(engine(calls, () => ({ status: 200, json: { ...LEVELS, level: "info", categories: { install: "error" } } })));
    expect(calls[1]).toMatchObject({ method: "PUT" });
    expect(calls[1].body).toEqual({ level: "info", categories: { install: "error" } });
    // A quieter global level stays too.
    useDiagSettingsStore.setState({
      engine: { level: "warn", categories: { rp: "debug" }, until_ms: null, raised: true, effective: {} },
    });
    await stopRaising(engine(calls, () => ({ status: 200, json: { ...LEVELS, level: "warn", categories: {} } })));
    expect(calls[2].body).toEqual({ level: "warn", categories: {} });
  });

  it("a refusal says why", async () => {
    await expect(
      changeLevels({ level: "info" }, engine([], () => ({ status: 400, json: { error: "unknown category" } }))),
    ).rejects.toThrow("unknown category");
  });
});
