import { invoke } from "@tauri-apps/api/core";

import { captureCrashReport } from "./crashReporter";

/** Discord support channel where users post packaged crash reports. */
export const DISCORD_REPORT_URL =
  "https://discord.com/channels/1464735724434624524/1465533832953462794";

export interface ReportResult {
  /** true = a zip was written and Discord opened. */
  ok: boolean;
  /** false + no error = the user cancelled the save dialog. */
  cancelled?: boolean;
  zipPath?: string;
  error?: string;
}

/**
 * One-click "report a problem" flow, callable from anywhere a failure
 * surfaces (the crash screen, an error toast, Settings):
 *
 *   1. Snapshot the current state into a fresh crash report (force past the
 *      debounce so the thing that just failed is definitely captured).
 *   2. Package every collected report into a single `.zip` the user picks.
 *   3. Open the Discord support channel so they can drag the zip in.
 *
 * Never throws — a reporting helper must not become a second failure.
 */
export async function reportProblem(
  trigger = "manual: report a problem",
): Promise<ReportResult> {
  try {
    // 1. Make sure the current failure is in the bundle.
    await captureCrashReport(trigger, { force: true });

    // 2. User picks where the zip goes.
    const { save } = await import("@tauri-apps/plugin-dialog");
    const dest = await save({
      defaultPath: `ps5upload-crash-reports-${Date.now()}.zip`,
      filters: [{ name: "Zip", extensions: ["zip"] }],
    });
    if (!dest || typeof dest !== "string") return { ok: false, cancelled: true };

    await invoke<number>("crash_reports_zip", { dest });

    // 3. Open the channel (best-effort; failing to open the browser must
    //    not fail the whole report — the zip is already saved).
    await openReportChannel();

    return { ok: true, zipPath: dest };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

/** Open the Discord support channel in the user's browser. Best-effort. */
export async function openReportChannel(): Promise<void> {
  try {
    const { open } = await import("@tauri-apps/plugin-shell");
    await open(DISCORD_REPORT_URL);
  } catch {
    // ignore — opening the browser is best-effort.
  }
}
