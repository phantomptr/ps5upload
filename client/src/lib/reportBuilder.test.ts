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
  engineLogText: async () => "[engine] connect 192.168.86.100 refused",
  appLogJsonl: () => "",
  currentAppLog: () => [],
}));
vi.mock("./diagnosticBundle", () => ({ buildDiagnosticBundle: () => ({ schema: 3 }) }));
vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine" }));

import { buildReport, TIMEOUTS, type BuildOptions, type SourceId } from "./reportBuilder";
import { EVENT_CATS } from "./eventRecord";
import type { ReportForm } from "./reportOutputs";

const form: ReportForm = {
  console: "192.168.86.100",
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
  consoles: ["192.168.86.100"],
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
        { ts: 1, src: "helper", cat: "helper", level: "error", code: "helper_fatal", console: "192.168.86.100", msg: "[fatal] signal 11" },
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
    expect(fetch).toHaveBeenCalledWith(expect.stringContaining("/api/ps5/helper-log-ftp?addr=192.168.86.100"), expect.anything());
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

  it("reads the console the report is about, not the selected tab", async () => {
    await buildReport(opts({ consoles: ["192.168.86.99"], sources: new Set<SourceId>(["console_logs"]) }));
    expect(snapshot).toHaveBeenCalledWith(expect.objectContaining({ host: "192.168.86.99" }));
  });

  it("an unreachable console's missing kernel and system logs are named in MISSING.txt", async () => {
    snapshot.mockResolvedValue({ snapshot: { errors: { connect: "refused" } }, klog: null, syslog: null, payload_logs: [] });
    const r = await buildReport(opts({ sources: new Set<SourceId>(["console_logs"]) }));
    expect(r.missing).toContainEqual({ source: "console_logs", reason: expect.stringContaining("refused") });
  });
});
