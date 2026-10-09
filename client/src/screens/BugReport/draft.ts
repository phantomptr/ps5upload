/**
 * The bug report wizard's state, kept per console until the report is built or discarded, so
 * closing the app mid-report loses nothing (spec §2).
 */
import { EVENT_CATS, type EventCat } from "../../lib/eventRecord";
import { SOURCE_IDS, type SourceId } from "../../lib/reportBuilder";
import type { ReportForm } from "../../lib/reportOutputs";

export type Step = 1 | 2 | 3 | 4;
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

export interface WizardDraft {
  step: Step;
  form: ReportForm;
  rangeKey: RangeKey;
  customStart: number | null;
  cats: EventCat[];
  sources: SourceId[];
  everything: boolean;
  redact: boolean;
}

export const MIN_DESCRIPTION = 20;
const KEY = "ps5upload.bugReportDraft.";

export function defaultDraft(appVersion: string, platform: string): WizardDraft {
  return {
    step: 1,
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
    cats: [...EVENT_CATS],
    sources: [...SOURCE_IDS],
    everything: true,
    redact: true,
  };
}

export function loadDraft(consoleKey: string): WizardDraft | null {
  try {
    const raw = localStorage.getItem(KEY + consoleKey);
    return raw ? (JSON.parse(raw) as WizardDraft) : null;
  } catch {
    return null;
  }
}

export function saveDraft(consoleKey: string, d: WizardDraft): void {
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
export function rangeStart(d: WizardDraft, now: number): number {
  if (d.rangeKey === "custom") return d.customStart ?? now - RANGE_MS["24h"];
  return now - RANGE_MS[d.rangeKey];
}

/** Whether the wizard may leave `step`. */
export function canAdvance(step: Step, d: WizardDraft): boolean {
  if (step === 1)
    return d.form.whatHappened.trim().length >= MIN_DESCRIPTION && (d.form.doing !== "other" || d.form.doingOther.trim().length > 0);
  if (step === 3) return d.sources.length > 0;
  return true;
}

/** A best guess at the issue form's platform; the user can change it in step 2. */
export function detectPlatform(isTauri: boolean): string {
  if (!isTauri) return "Browser / self-hosted web UI";
  const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";
  if (/Android/i.test(ua)) return "Android";
  if (/Windows/i.test(ua)) return /ARM64|aarch64/i.test(ua) ? "Windows (ARM64)" : "Windows (x64)";
  if (/Mac/i.test(ua)) return "macOS (Apple Silicon)";
  if (/Linux/i.test(ua)) return /aarch64|arm64/i.test(ua) ? "Linux (ARM64)" : "Linux (x64)";
  return PLATFORMS[0];
}
