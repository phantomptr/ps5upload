import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test" }));

import { collectEngineDiagnostics } from "./engineDiagnostics";

function stubFetch(routes: Record<string, unknown>) {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      calls.push(url);
      const path = url.replace("http://engine.test", "");
      if (!(path in routes)) return { ok: false, status: 404, json: async () => ({}) };
      return { ok: true, status: 200, json: async () => routes[path] };
    }),
  );
  return calls;
}

describe("collectEngineDiagnostics — install history (spec §6)", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("attaches each known console's unified install history", async () => {
    const entry = { job: "17-1", verdict: "failed", code: 2158630511 };
    stubFetch({
      "/api/jobs": [],
      "/api/pkg/install/sessions": [],
      "/api/pkg/install/history?ps5_addr=192.168.0.99": [entry],
      "/api/pkg/install/history?ps5_addr=192.168.0.100": [],
    });
    const d = await collectEngineDiagnostics({
      consoles: ["192.168.0.99", "192.168.0.100"],
      redact: false,
    });
    expect(d.install_history).toEqual([
      { console: "192.168.0.99", entries: [entry] },
      { console: "192.168.0.100", entries: [] },
    ]);
  });

  it("redacts the console address and dedupes host:port forms", async () => {
    const calls = stubFetch({
      "/api/jobs": [],
      "/api/pkg/install/sessions": [],
      "/api/pkg/install/history?ps5_addr=192.168.0.99": [],
    });
    const d = await collectEngineDiagnostics({
      consoles: ["192.168.0.99", "192.168.0.99:9114", ""],
      redact: true,
    });
    expect(d.install_history).toEqual([{ console: "<IPv4>", entries: [] }]);
    expect(calls.filter((c) => c.includes("/history")).length).toBe(1);
  });

  it("records a failed history probe instead of dropping it silently", async () => {
    stubFetch({ "/api/jobs": [], "/api/pkg/install/sessions": [] });
    const d = await collectEngineDiagnostics({ consoles: ["10.0.0.5"], redact: false });
    expect(d.install_history).toEqual([{ console: "10.0.0.5", entries: null }]);
    expect(Object.keys(d.errors)).toContain("install_history:10.0.0.5");
  });
});
