/**
 * Collects everything a bug report carries (spec §3.1, §4.1). Every selected source runs under
 * its own time limit; one that fails or times out is named in MISSING.txt with the reason, and
 * the rest of the report is built anyway. Nothing here redacts: the save step does it once over
 * every file (Rust on desktop, `createRedactor` in the browser), so an address has the same
 * `<ip-N>` everywhere.
 */
import type { BundleEntry } from "./browserBugBundle";
import { appLogJsonl, currentAppLog, engineLogText } from "./browserBugBundle";
import { buildDiagnosticBundle } from "./diagnosticBundle";
import { collectEngineDiagnostics } from "./engineDiagnostics";
import type { EventCat } from "./eventRecord";
import { buildPs5Snapshot } from "./ps5Snapshot";
import { reportMarkdown, type ReportForm } from "./reportOutputs";
import { fetchTimeline, filterCats, formatLine } from "./reportTimeline";
import { getEngineUrl } from "../state/engine";

export type SourceId =
  | "app_journal"
  | "engine_journal"
  | "engine_log"
  | "app_log"
  | "crash_reports"
  | "helper_log"
  | "helper_ftp"
  | "console_logs"
  | "jobs"
  | "settings";
export const SOURCE_IDS: readonly SourceId[] = [
  "app_journal",
  "engine_journal",
  "engine_log",
  "app_log",
  "crash_reports",
  "helper_log",
  "helper_ftp",
  "console_logs",
  "jobs",
  "settings",
];

/** Milliseconds each source may take. The console snapshot makes several calls, so it gets longer. */
export const TIMEOUTS = { snapshot: 12_000, ftp: 9_000, engine: 5_000 } as const;
/** Above this the oldest timeline lines are dropped (the zip cap is 64 MB). */
const TIMELINE_MAX_BYTES = 40 << 20;

export interface BuildOptions {
  form: ReportForm;
  since: number;
  until: number;
  cats: ReadonlySet<EventCat>;
  sources: ReadonlySet<SourceId>;
  /** Console addresses the report is about (the first is the form's console). */
  consoles: string[];
  /** Attached screenshots (browser) as base64. Desktop passes gallery paths to the save step. */
  images: { name: string; base64: string }[];
  /** Desktop reads its own log files from disk; the browser has to send them. */
  platform: "desktop" | "browser";
  onProgress?(id: SourceId, state: "running" | "ok" | "missing"): void;
}

export interface Missing {
  source: SourceId;
  reason: string;
}

export interface BuiltReport {
  entries: BundleEntry[];
  missing: Missing[];
  filename: string;
  /** The last warnings and errors, formatted: the GitHub issue's log excerpt. */
  problemLines: string[];
  /** What the desktop builder reads from disk itself (its include flags). */
  desktop: { engine_log: boolean; app_logs: boolean; crash_reports: boolean };
}

export function withTimeout<T>(p: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`${label} timed out after ${Math.round(ms / 1000)} s`)), ms);
    p.then(
      (v) => {
        clearTimeout(t);
        resolve(v);
      },
      (e) => {
        clearTimeout(t);
        reject(e instanceof Error ? e : new Error(String(e)));
      },
    );
  });
}

function stamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
}

/** The console's folder in the zip. Not named after its address: zip paths are not redacted. */
function consoleDir(_host: string): string {
  return "console";
}

function settingsSnapshot(): Record<string, string> {
  const out: Record<string, string> = {};
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      // Report drafts are the form itself, already in report.json; the console list holds the
      // Remote Play wake keys (report.json's diagnostic carries it without them).
      if (k && k.startsWith("ps5upload.") && !k.startsWith("ps5upload.bugReportDraft.") && !k.startsWith("ps5upload.roster."))
        out[k] = localStorage.getItem(k) ?? "";
    }
  } catch {
    // storage unavailable: an empty settings file says so
  }
  return out;
}

const README = `ps5upload bug report
====================

report.md         What happened, in the reporter's words, and the environment.
report.json       The same, machine-readable, plus engine and console state.
timeline.txt      App, engine and PS5 helper events for the chosen time range, oldest first.
timeline.jsonl    The same events, one JSON object per line.
MISSING.txt       Everything that was asked for but could not be read, and why.
console/          Logs read from the PS5: the helper's stderr.log, klog, syslog.
logs/             The app's and the engine's own logs.
screenshots/      Pictures the reporter attached.

Addresses, MAC addresses, home folders and serials are replaced when redaction is on
(<ip-1> is the same console in every file); pairing keys and tokens are always removed.
`;

export async function buildReport(o: BuildOptions): Promise<BuiltReport> {
  const entries: BundleEntry[] = [];
  const missing: Missing[] = [];
  const want = (s: SourceId) => o.sources.has(s);
  const run = async <T>(id: SourceId, ms: number, work: () => Promise<T>): Promise<T | null> => {
    o.onProgress?.(id, "running");
    try {
      const v = await withTimeout(work(), ms, id);
      o.onProgress?.(id, "ok");
      return v;
    } catch (e) {
      missing.push({ source: id, reason: e instanceof Error ? e.message : String(e) });
      o.onProgress?.(id, "missing");
      return null;
    }
  };

  // Timeline: the two journals, merged.
  let problemLines: string[] = [];
  let dropped = { engine: 0, app: 0 };
  if (want("engine_journal") || want("app_journal")) {
    const tl = await run("engine_journal", TIMEOUTS.engine + 1_000, () => fetchTimeline(o.since, o.until));
    if (tl) {
      if (tl.engineError && want("engine_journal")) missing.push({ source: "engine_journal", reason: tl.engineError });
      dropped = { engine: tl.engineDropped, app: tl.appDropped };
      let events = filterCats(tl.events, o.cats).filter(
        (e) => (e.src === "app" ? want("app_journal") : want("engine_journal")),
      );
      let jsonl = events.map((e) => JSON.stringify(e)).join("\n");
      let trimmed = 0;
      while (jsonl.length > TIMELINE_MAX_BYTES && events.length > 0) {
        const cut = Math.ceil(events.length / 10);
        events = events.slice(cut);
        trimmed += cut;
        jsonl = events.map((e) => JSON.stringify(e)).join("\n");
      }
      const text = events.map((e) => formatLine(e)).join("\n");
      entries.push({ path: "timeline.txt", text: trimmed ? `(${trimmed} oldest events left out to fit)\n${text}` : text });
      entries.push({ path: "timeline.jsonl", text: jsonl });
      problemLines = events
        .filter((e) => e.level !== "info")
        .slice(-30)
        .map((e) => formatLine(e));
    }
  }

  // The console: helper logs (live), klog/syslog, its snapshot for report.json.
  let ps5: unknown = null;
  if (want("helper_log") || want("console_logs")) {
    const snap = await run(want("console_logs") ? "console_logs" : "helper_log", TIMEOUTS.snapshot, () =>
      buildPs5Snapshot({ redact: false, host: o.consoles[0] }),
    );
    const host = o.consoles[0] ?? "console";
    if (snap) {
      ps5 = snap.snapshot;
      if (want("console_logs") && !snap.klog && !snap.syslog) {
        // An unreachable console is not an error from the snapshot: it just has nothing to read.
        const errs = (snap.snapshot as { errors?: Record<string, string> }).errors ?? {};
        const why = Object.entries(errs).map(([k, v]) => `${k}: ${v}`).join("; ");
        missing.push({ source: "console_logs", reason: why || "the console was not connected" });
      }
      if (want("console_logs")) {
        if (snap.klog) entries.push({ path: `${consoleDir(host)}/klog.txt`, text: snap.klog });
        if (snap.syslog) entries.push({ path: `${consoleDir(host)}/syslog.txt`, text: snap.syslog });
      }
      if (want("helper_log"))
        for (const f of snap.payload_logs) entries.push({ path: `${consoleDir(host)}/${f.name}`, text: f.text });
    }
    const gotStderr = !!snap?.payload_logs.some((f) => f.name === "stderr.log" && f.text);
    if (want("helper_log") && !gotStderr) {
      if (want("helper_ftp") && o.consoles[0]) {
        const ftp = await run("helper_ftp", TIMEOUTS.ftp, async () => {
          const r = await fetch(`${getEngineUrl()}/api/ps5/helper-log-ftp?addr=${encodeURIComponent(host)}`, {
            signal: AbortSignal.timeout(TIMEOUTS.ftp),
          });
          const body = (await r.json()) as { stderr?: string; stderr_old?: string; error?: string };
          if (!r.ok) throw new Error(body.error ?? `HTTP ${r.status}`);
          return body;
        });
        if (ftp?.stderr) entries.push({ path: `${consoleDir(host)}/stderr.log`, text: ftp.stderr });
        if (ftp?.stderr_old) entries.push({ path: `${consoleDir(host)}/stderr_old.log`, text: ftp.stderr_old });
      } else if (!missing.some((m) => m.source === "helper_log" || m.source === "console_logs")) {
        missing.push({ source: "helper_log", reason: "the helper did not return its stderr.log" });
      }
    }
  }

  // Engine state: jobs, install sessions, install history.
  const engine = want("jobs")
    ? await run("jobs", TIMEOUTS.engine, () => collectEngineDiagnostics({ consoles: o.consoles, redact: false }))
    : null;

  // Logs the browser has to send; the desktop builder reads them from disk.
  const desktop = { engine_log: false, app_logs: false, crash_reports: false };
  if (o.platform === "desktop") {
    desktop.engine_log = want("engine_log");
    desktop.app_logs = want("app_log");
    desktop.crash_reports = want("crash_reports");
  } else {
    if (want("engine_log")) {
      const t = await run("engine_log", TIMEOUTS.engine, () => engineLogText());
      if (t !== null) entries.push({ path: "logs/engine.log", text: t });
    }
    if (want("app_log"))
      entries.push({ path: "logs/app.jsonl", text: appLogJsonl(currentAppLog(), Math.ceil((o.until - o.since) / 60_000)) });
    if (want("crash_reports"))
      missing.push({ source: "crash_reports", reason: "only the desktop app keeps crash reports" });
  }

  if (want("settings")) entries.push({ path: "settings.json", text: JSON.stringify(settingsSnapshot(), null, 2) });
  for (const img of o.images)
    entries.push({ path: `screenshots/${img.name.replace(/[^A-Za-z0-9._-]/g, "_")}`, base64: img.base64 });

  const manifest = {
    schema: 2,
    kind: "ps5upload-bug-report",
    generated_at: new Date().toISOString(),
    form: { ...o.form, pinned: o.form.pinned },
    range: { since: o.since, until: o.until },
    categories: [...o.cats],
    sources: [...o.sources],
    dropped,
    diagnostic: buildDiagnosticBundle({ appVersion: o.form.appVersion, redact: false, logLimit: 200 }),
    engine,
    ps5,
  };
  entries.unshift(
    { path: "README.txt", text: README },
    { path: "report.md", text: reportMarkdown(o.form, problemLines) },
    { path: "report.json", text: JSON.stringify(manifest, null, 2) },
  );
  entries.push({
    path: "MISSING.txt",
    text: missing.length ? missing.map((m) => `${m.source}: ${m.reason}`).join("\n") : "Nothing missing.",
  });

  return {
    entries,
    missing,
    filename: `ps5upload-report-${stamp(new Date())}.zip`,
    problemLines,
    desktop,
  };
}
