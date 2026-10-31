import { ExternalLink, Settings as SettingsIcon } from "lucide-react";

import type { UpdateCheck } from "../api/ps5";
import { useTr } from "../state/lang";
import {
  openApkDownloadPage,
  openInstallSettings,
  progressPercent,
  useApkUpdateStore,
} from "../state/apkUpdate";

/**
 * Android in-app update progress (Settings → Updates): the download bar, the
 * "Install unknown apps" notice, the wait for Android's dialog, and the
 * "Open download page" fallback once something failed. Renders nothing when
 * idle and nothing failed.
 */
export function ApkUpdateStatus({
  result,
  failed,
}: {
  result: UpdateCheck;
  /** The update store already shows a failure (download-failed). */
  failed: boolean;
}) {
  const tr = useTr();
  const step = useApkUpdateStore((s) => s.step);
  const showFallback = failed || step.kind === "failed";

  return (
    <div className="flex flex-col gap-2 text-xs">
      {step.kind === "downloading" && <ApkProgress received={step.received} total={step.total} />}
      {step.kind === "needs-permission" && (
        <div className="flex flex-wrap items-center gap-2 text-[var(--color-text)]">
          <span>
            {tr(
              "update_apk_needs_permission",
              undefined,
              "Allow PS5Upload to install apps in the setting that opened, then come back here.",
            )}
          </span>
          <button
            type="button"
            onClick={openInstallSettings}
            className="inline-flex items-center gap-1 underline-offset-2 hover:underline max-md:min-h-11"
          >
            <SettingsIcon size={12} />
            {tr("update_apk_open_setting", undefined, "Open setting")}
          </button>
        </div>
      )}
      {step.kind === "installing" && (
        <div className="text-[var(--color-text)]">
          {tr("update_apk_confirm", undefined, "Tap Update in Android's dialog to finish.")}
        </div>
      )}
      {step.kind === "failed" && !failed && (
        <div className="text-[var(--color-bad)]">
          {step.during === "download"
            ? tr("update_download_error", { message: step.message }, `Download failed: ${step.message}`)
            : tr("update_install_error", { message: step.message }, `The update didn't install: ${step.message}`)}
        </div>
      )}
      {showFallback && (
        <button
          type="button"
          onClick={() => void openApkDownloadPage(result)}
          className="inline-flex w-fit items-center gap-1.5 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 hover:bg-[var(--color-surface-3)] max-md:min-h-11"
        >
          <ExternalLink size={12} />
          {tr("update_open_release_page", undefined, "Open download page")}
        </button>
      )}
    </div>
  );
}

/** Download bar with a percent; indeterminate while the size is unknown. */
export function ApkProgress({ received, total }: { received: number; total: number }) {
  const tr = useTr();
  const pct = progressPercent(received, total);
  return (
    <div className="flex flex-col gap-1">
      <div
        className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--color-surface-3)]"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct ?? undefined}
      >
        <div
          className="h-full rounded-full bg-[var(--color-accent)] transition-[width]"
          style={{ width: `${pct ?? 0}%` }}
        />
      </div>
      <span className="text-[var(--color-muted)]">
        {pct == null
          ? tr("update_toast_downloading", undefined, "Downloading…")
          : tr("update_apk_downloading_pct", { percent: pct }, "Downloading… {percent}%")}
      </span>
    </div>
  );
}
