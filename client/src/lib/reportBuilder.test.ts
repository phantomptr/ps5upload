import { beforeEach, describe, expect, it, vi } from "vitest";

const snapshot = vi.fn();
const engineDiag = vi.fn();
const timeline = vi.fn();
vi.mock("./ps5Snapshot", () => ({ buildPs5Snapshot: (...a: unknown[]) => snapshot(...a) }));
vi.mock("./engineDiagnostics", () => ({ collectEngineDiagnostics: (...a: unknown[]) => engineDiag(...a) }));
vi.mock("./reportTimeline", async (orig) => ({
  ...(await orig<typeof import("./reportTimeline")>()),
  fetchTimeline: (...a: unknown[]) => timeline(...a),
}));
vi.mock("./browserBugBundle", () => ({
  engineLogText: async () => "[engine] connect 192.0.2.100 refused",
  appLogJsonl: () => "",
  currentAppLog: () => [],
}));
vi.mock("./diagnosticBundle", () => ({ buildDiagnosticBundle: () => ({ schema: 3 }) }));
vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine" }));

import { buildReport, TIMEOUTS, type BuildOptions, type SourceId } from "./reportBuilder";
import { EVENT_CATS } from "./eventRecord";
import type { ReportForm } from "./reportOutputs";

const form: ReportForm = {
  console: "192.0.2.100",
  doing: "connecting",
  doingOther: "",
  whatHappened: "It disconnected and won't come back",
  pinned: [],
  steps: "",
  frequency: "",
  started: "",
  appVersion: "6.5.2",
  platform: "macOS (Apple Silicon)",
  firmware: "13.60",
  model: "",
  payloads: "",
  githubIssueUrl: "",
};

const opts = (over: Partial<BuildOptions> = {}): BuildOptions => ({
  form,
  since: 0,
  until: Date.now(),
  cats: new Set(EVENT_CATS),
  sources: new Set<SourceId>(["engine_journal", "app_journal"]),
  consoles: ["192.0.2.100"],
  images: [],
  platform: "browser",
  ...over,
});

const paths = (r: { entries: { path: string }[] }) => r.entries.map((e) => e.path);

describe("buildReport", () => {
  beforeEach(() => {
    vi.useRealTimers();
    for (const f of [snapshot, engineDiag, timeline]) f.mockReset();
    timeline.mockResolvedValue({
      events: [
        { ts: 1, src: "helper", cat: "helper", level: "error", code: "helper_fatal", console: "192.0.2.100", msg: "[fatal] signal 11" },
      ],
      engineDropped: 0,
      appDropped: 0,
      engineError: null,
    });
    engineDiag.mockResolvedValue({ jobs: [] });
    snapshot.mockResolvedValue({ snapshot: {}, klog: "klog", syslog: null, payload_logs: [{ name: "stderr.log", text: "ready" }] });
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ stderr: "from ftp", stderr_old: "" }))));
  });

  it("keeps two screenshots with the same name apart", async () => {
    // Two "Screenshot.png" from different folders made one path twice, and the zip refused it.
    const r = await buildReport(
      opts({ images: [{ name: "Screenshot.png", base64: "AA==" }, { name: "Screenshot.png", base64: "AQ==" }] }),
    );
    const shots = paths(r).filter((p) => p.startsWith("screenshots/"));
    expect(shots).toHaveLength(2);
    expect(new Set(shots).size).toBe(2);
  });

  it("always writes the report files, and only the chosen sources", async () => {
    const r = await buildReport(opts());
    expect(paths(r)).toEqual(
      expect.arrayContaining(["README.txt", "report.md", "report.json", "timeline.txt", "timeline.jsonl", "MISSING.txt"]),
    );
    expect(snapshot).not.toHaveBeenCalled();
    expect(r.entries.find((e) => e.path === "timeline.txt")?.text).toContain("[fatal] signal 11");
    expect(r.problemLines.join("\n")).toContain("signal 11");
  });

  it("a console that never answers ends up in MISSING.txt within its timeout", async () => {
    vi.useFakeTimers();
    snapshot.mockReturnValue(new Promise(() => {}));
    const p = buildReport(opts({ sources: new Set<SourceId>(["console_logs", "engine_journal"]) }));
    await vi.advanceTimersByTimeAsync(TIMEOUTS.snapshot + 10);
    const r = await p;
    expect(r.missing).toContainEqual({ source: "console_logs", reason: expect.stringContaining("timed out") });
    expect(r.entries.find((e) => e.path === "MISSING.txt")?.text).toContain("console_logs");
  });

  it("reads the helper log over FTP only when the live read did not get it", async () => {
    await buildReport(opts({ sources: new Set<SourceId>(["helper_log", "helper_ftp"]) }));
    expect(fetch).not.toHaveBeenCalled();

    snapshot.mockResolvedValue({ snapshot: {}, klog: null, syslog: null, payload_logs: [] });
    const r = await buildReport(opts({ sources: new Set<SourceId>(["helper_log", "helper_ftp"]) }));
    expect(fetch).toHaveBeenCalledWith(expect.stringContaining("/api/ps5/helper-log-ftp?addr=192.0.2.100"), expect.anything());
    expect(r.entries.find((e) => e.path.endsWith("stderr.log"))?.text).toBe("from ftp");
  });

  it("names what is missing on the browser that only the desktop can read", async () => {
    const r = await buildReport(opts({ sources: new Set<SourceId>(["crash_reports"]) }));
    expect(r.missing).toContainEqual({ source: "crash_reports", reason: expect.stringContaining("desktop") });
  });

  it("leaves desktop-read files to the desktop builder", async () => {
    const r = await buildReport(opts({ platform: "desktop", sources: new Set<SourceId>(["engine_log", "app_log", "crash_reports"]) }));
    expect(paths(r)).not.toContain("logs/engine.log");
    expect(r.desktop).toEqual({ engine_log: true, app_logs: true, crash_reports: true });
  });

  it("a failing engine journal is missing, not fatal", async () => {
    timeline.mockResolvedValue({ events: [], engineDropped: 0, appDropped: 0, engineError: "HTTP 502" });
    const r = await buildReport(opts());
    expect(r.missing).toContainEqual({ source: "engine_journal", reason: "HTTP 502" });
  });

  it("settings.json leaves out the console list, which holds the wake keys", async () => {
    const mem = new Map([
      ["ps5upload.roster.v1", '{"profiles":[{"wake_rp_key":"rp"}]}'],
      ["ps5upload.theme", "dark"],
    ]);
    vi.stubGlobal("localStorage", {
      get length() {
        return mem.size;
      },
      key: (i: number) => [...mem.keys()][i] ?? null,
      getItem: (k: string) => mem.get(k) ?? null,
    });
    const r = await buildReport(opts({ sources: new Set<SourceId>(["settings"]) }));
    const settings = r.entries.find((e) => e.path === "settings.json")?.text ?? "";
    expect(settings).toContain("ps5upload.theme");
    expect(settings).not.toContain("roster");
    expect(settings).not.toContain("wake_rp_key");
  });

  it("settings.json summarizes the mesh state, whose console fields hold the wake keys", async () => {
    // The mesh keeps each console field as "c:<address>:<field>", out of reach of the
    // key-name redaction: copied as is, a console's Remote Play keys went out in settings.json.
    const mesh = JSON.stringify({
      device: "d1",
      clock: { t: 1, c: 0 },
      m: {
        "c:192.168.0.100": { v: 1, h: {} },
        "c:192.168.0.100:wake_regist_key": { v: "FAKEREGISTKEY0123", h: {} },
        "c:192.168.0.100:wake_rp_key": { v: "FAKERPKEY4567", h: {} },
        "p:ps5upload.theme": { v: "dark", h: {} },
      },
    });
    const mem = new Map([
      ["ps5upload.mesh_state.v2", mesh],
      ["ps5upload.detailed_logs_until", "1800000000000"],
    ]);
    vi.stubGlobal("localStorage", {
      get length() {
        return mem.size;
      },
      key: (i: number) => [...mem.keys()][i] ?? null,
      getItem: (k: string) => mem.get(k) ?? null,
    });
    const r = await buildReport(opts({ sources: new Set<SourceId>(["settings"]) }));
    const settings = r.entries.find((e) => e.path === "settings.json")?.text ?? "";
    expect(settings).not.toContain("FAKEREGISTKEY");
    expect(settings).not.toContain("FAKERPKEY");
    expect(JSON.parse(settings)["ps5upload.mesh_state.v2"]).toBe("<summary: 4 entries, 1 consoles>");
    // Whether Detailed logs were on rides along with the other settings.
    expect(settings).toContain("ps5upload.detailed_logs_until");
  });

  it("report.json carries the engine's feature summary and, on desktop, the link status", async () => {
    const features = { play_together: { parties: [{ open: true, players_in: 1 }] } };
    engineDiag.mockResolvedValue({ jobs: [], features });
    const r = await buildReport(opts({ platform: "desktop", sources: new Set<SourceId>(["jobs"]) }));
    const report = JSON.parse(r.entries.find((e) => e.path === "report.json")?.text ?? "{}");
    expect(report.engine.features).toEqual(features);
    // The diagnostic mock has no app_links: the shell's status is then simply not added.
    expect(report.diagnostic).toEqual({ schema: 3 });
  });

  it("reads the console the report is about, not the selected tab", async () => {
    await buildReport(opts({ consoles: ["192.0.2.99"], sources: new Set<SourceId>(["console_logs"]) }));
    expect(snapshot).toHaveBeenCalledWith(expect.objectContaining({ host: "192.0.2.99" }));
  });

  it("an unreachable console's missing kernel and system logs are named in MISSING.txt", async () => {
    snapshot.mockResolvedValue({ snapshot: { errors: { connect: "refused" } }, klog: null, syslog: null, payload_logs: [] });
    const r = await buildReport(opts({ sources: new Set<SourceId>(["console_logs"]) }));
    expect(r.missing).toContainEqual({ source: "console_logs", reason: expect.stringContaining("refused") });
  });
});

describe("bounded logs and the new sections", () => {
  beforeEach(() => {
    vi.useRealTimers();
    timeline.mockResolvedValue({ events: [], engineDropped: 0, appDropped: 0, engineError: null });
  });

  it("keepNewest keeps the end, from a line start", async () => {
    const { keepNewest } = await import("./reportBuilder");
    expect(keepNewest("abc", 10)).toEqual({ text: "abc", cut: 0 });
    const r = keepNewest("old line\nmiddle\nnewest\n", 12);
    expect(r.text).toBe("newest\n");
    expect(r.cut).toBeGreaterThan(0);
  });

  it("a console log over the cap keeps its newest part, and MISSING.txt says so", async () => {
    const { LOG_MAX_BYTES } = await import("./reportBuilder");
    const big = "old\n".repeat(LOG_MAX_BYTES / 2) + "the last line\n";
    snapshot.mockResolvedValue({ snapshot: {}, klog: big, syslog: null, payload_logs: [] });
    const r = await buildReport(opts({ sources: new Set<SourceId>(["console_logs"]) }));
    const klog = r.entries.find((e) => e.path === "console/klog.txt")?.text ?? "";
    expect(klog.length).toBeLessThanOrEqual(LOG_MAX_BYTES);
    expect(klog.endsWith("the last line\n")).toBe(true);
    expect(r.entries.find((e) => e.path === "MISSING.txt")?.text).toContain("cut to fit: console/klog.txt");
  });

  it("report.json says which log levels were in force and carries the client's Remote Play sessions", async () => {
    const r = await buildReport(opts({ sources: new Set<SourceId>(["system"]) }));
    const report = JSON.parse(r.entries.find((e) => e.path === "report.json")?.text ?? "{}");
    expect(report.log_levels).toMatchObject({ level: expect.any(String), raised: expect.any(Boolean) });
    expect(Array.isArray(report.remote_play_client)).toBe(true);
    // The browser build: what the page itself can see.
    expect(report.system).toMatchObject({ source: "browser" });
  });
});

describe("scrubUrls", () => {
  it("keeps a link's host and path, never its query or fragment", async () => {
    const { scrubUrls } = await import("./reportBuilder");
    const stored = JSON.stringify([
      "https://cdn.example.com/game.pkg?X-Amz-Signature=abc123&token=zz",
      "http://192.168.1.20:19113/join#i=0f0f0f0f0f0f0f0f",
      "http://10.0.0.5:19113/pad#t=deadbeefdeadbeef&w=1",
      "https://example.com/plain/path",
    ]);
    const out = scrubUrls(stored);
    for (const secret of ["abc123", "zz", "0f0f0f0f0f0f0f0f", "deadbeefdeadbeef"]) expect(out).not.toContain(secret);
    expect(out).toContain("https://cdn.example.com/game.pkg?<removed>");
    expect(out).toContain("http://192.168.1.20:19113/join#<removed>");
    expect(out).toContain("https://example.com/plain/path");
  });
});
