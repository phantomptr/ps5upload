import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { DETAILED_LOGS_MS, useDiagSettingsStore } from "../state/diagSettings";
import { installDetailedLogs, sendDetailedToEngine } from "./detailedLogs";

type Call = { url: string; body: unknown };

function fakeFetch(calls: Call[], ok = true): typeof fetch {
  return (async (url: string, init?: RequestInit) => {
    calls.push({ url: String(url), body: JSON.parse(String(init?.body ?? "null")) });
    return { ok } as Response;
  }) as unknown as typeof fetch;
}

describe("sendDetailedToEngine", () => {
  it("sends the end time when on and only 'off' when off", async () => {
    const calls: Call[] = [];
    expect(await sendDetailedToEngine(123, fakeFetch(calls))).toBe(true);
    expect(await sendDetailedToEngine(null, fakeFetch(calls))).toBe(true);
    expect(calls[0].url).toMatch(/\/api\/diag\/log-level$/);
    expect(calls[0].body).toEqual({ detailed: true, until_ms: 123 });
    expect(calls[1].body).toEqual({ detailed: false });
  });

  it("an engine that cannot be reached is not an error", async () => {
    const failing = (async () => {
      throw new Error("down");
    }) as unknown as typeof fetch;
    expect(await sendDetailedToEngine(1, failing)).toBe(false);
  });
});

describe("installDetailedLogs", () => {
  const calls: Call[] = [];
  let stop: () => void = () => {};
  beforeEach(() => {
    vi.useFakeTimers();
    calls.length = 0;
    vi.stubGlobal("fetch", fakeFetch(calls));
    useDiagSettingsStore.setState({ detailedUntil: null });
  });
  afterEach(() => {
    stop();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it("tells the engine on a change and turns itself off when its time is up", async () => {
    stop = installDetailedLogs();
    expect(calls).toHaveLength(0); // off at start: the engine is left alone
    useDiagSettingsStore.getState().setDetailed(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(calls[calls.length - 1]?.body).toMatchObject({ detailed: true });
    await vi.advanceTimersByTimeAsync(DETAILED_LOGS_MS + 1);
    expect(useDiagSettingsStore.getState().detailedUntil).toBeNull();
    expect(calls[calls.length - 1]?.body).toEqual({ detailed: false });
  });

  it("an app started while on tells the engine again", async () => {
    useDiagSettingsStore.setState({ detailedUntil: Date.now() + 60_000 });
    stop = installDetailedLogs();
    await vi.advanceTimersByTimeAsync(0);
    expect(calls).toHaveLength(1);
    expect(calls[0].body).toMatchObject({ detailed: true });
  });
});
