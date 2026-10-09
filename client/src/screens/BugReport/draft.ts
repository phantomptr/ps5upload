/**
 * The bug report form's state, kept per console until the report is built or discarded, so
 * closing the app mid-report loses nothing. Everything recorded in the time frame goes in: the
 * only choice about the logs is how far back.
 */
import type { ReportForm } from "../../lib/reportOutputs";

export type RangeKey = "15m" | "1h" | "6h" | "24h" | "3d" | "7d" | "custom";
export const RANGE_KEYS: readonly RangeKey[] = ["15m", "1h", "6h", "24h", "3d", "7d", "custom"];
export const RANGE_MS: Record<Exclude<RangeKey, "custom">, number> = {
  "15m": 15 * 60_000,
  "1h": 3_600_000,
  "6h": 6 * 3_600_000,
  "24h": 86_400_000,
  "3d": 3 * 86_400_000,
  "7d": 7 * 86_400_000,
};

/** The issue form's platform options, verbatim (.github/ISSUE_TEMPLATE/bug_report.yml). */
export const PLATFORMS = [
  "macOS (Apple Silicon)",
  "macOS (Intel)",
  "Windows (x64)",
  "Windows (ARM64)",
  "Linux (x64)",
  "Linux (ARM64)",
  "Android",
  "Browser / self-hosted web UI",
] as const;

export interface ReportDraft {
  form: ReportForm;
  rangeKey: RangeKey;
  customStart: number | null;
}

/** Enough for "App froze": the logs carry the rest. */
export const MIN_DESCRIPTION = 8;
const KEY = "ps5upload.bugReportDraft.";

export function defaultDraft(appVersion: string, platform: string): ReportDraft {
  return {
    form: {
      console: null,
      doing: "connecting",
      doingOther: "",
      whatHappened: "",
      pinned: [],
      steps: "",
      frequency: "",
      started: "",
      appVersion,
      platform,
      firmware: "",
      model: "",
      payloads: "",
      githubIssueUrl: "",
    },
    rangeKey: "24h",
    customStart: null,
  };
}

export function loadDraft(consoleKey: string): ReportDraft | null {
  try {
    const raw = localStorage.getItem(KEY + consoleKey);
    if (!raw) return null;
    // Only the fields this form has: a draft from the old four-step wizard carries more.
    const { form, rangeKey, customStart } = JSON.parse(raw) as ReportDraft;
    return form ? { form, rangeKey: rangeKey ?? "24h", customStart: customStart ?? null } : null;
  } catch {
    return null;
  }
}

export function saveDraft(consoleKey: string, d: ReportDraft): void {
  try {
    localStorage.setItem(KEY + consoleKey, JSON.stringify(d));
  } catch {
    // storage unavailable: the draft just isn't kept
  }
}

export function clearDraft(consoleKey: string): void {
  try {
    localStorage.removeItem(KEY + consoleKey);
  } catch {
    // nothing to clear
  }
}

/** Where the report's time range starts; it always ends now. */
export function rangeStart(d: ReportDraft, now: number): number {
  if (d.rangeKey === "custom") return d.customStart ?? now - RANGE_MS["24h"];
  return now - RANGE_MS[d.rangeKey];
}

/** What still stops the report from being built, or null when it's ready. */
export function whatIsMissing(d: ReportDraft): "description" | "doing_other" | null {
  if (d.form.whatHappened.trim().length < MIN_DESCRIPTION) return "description";
  if (d.form.doing === "other" && !d.form.doingOther.trim()) return "doing_other";
  return null;
}

/** A best guess at the issue form's platform; the user can change it. */
export function detectPlatform(isTauri: boolean): string {
  if (!isTauri) return "Browser / self-hosted web UI";
  const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";
  if (/Android/i.test(ua)) return "Android";
  if (/Windows/i.test(ua)) return /ARM64|aarch64/i.test(ua) ? "Windows (ARM64)" : "Windows (x64)";
  if (/Mac/i.test(ua)) return "macOS (Apple Silicon)";
  if (/Linux/i.test(ua)) return /aarch64|arm64/i.test(ua) ? "Linux (ARM64)" : "Linux (x64)";
  return PLATFORMS[0];
}

/** The issue form's platform for the OS and CPU the desktop app reports (`host_platform`);
 *  null when it isn't one of the options. The web view alone can't tell Intel from Apple Silicon. */
export function platformFromHost(h: { os: string; arch: string }): string | null {
  const arm = h.arch === "aarch64" || h.arch === "arm";
  switch (h.os) {
    case "macos":
      return arm ? "macOS (Apple Silicon)" : "macOS (Intel)";
    case "windows":
      return arm ? "Windows (ARM64)" : "Windows (x64)";
    case "linux":
      return arm ? "Linux (ARM64)" : "Linux (x64)";
    case "android":
      return "Android";
    default:
      return null;
  }
}
