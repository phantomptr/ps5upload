import { useState } from "react";
import { useNavigate } from "react-router";
import { Download, X, Sparkles } from "lucide-react";

import { useUpdateStore } from "../state/update";
import { useTr } from "../state/lang";
import { Spinner } from "../components/Spinner";

/**
 * Non-intrusive update banner. The update store already auto-checks on launch
 * (AppShell's useUpdateCheckOnMount); this surfaces the "available" result as a
 * slim, dismissible bar at the top of the content area with a one-tap Download
 * — instead of only a sidebar dot the user has to notice. Dismiss hides it for
 * the session (the Settings → Updates panel remains the durable surface).
 *
 * Renders nothing unless an update is available (or actively downloading after
 * the user clicked Download here), so it costs nothing in the common case.
 */
export function UpdateToast() {
  const tr = useTr();
  const navigate = useNavigate();
  const phase = useUpdateStore((s) => s.phase);
  const download = useUpdateStore((s) => s.download);
  // Dismiss is keyed by version so a NEWER release later in the session
  // re-surfaces the bar instead of staying hidden.
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);

  // Surface every actionable update state — not just "available" — so the bar
  // doesn't vanish mid-flow (downloading), on completion (downloaded, which
  // needs an install step), or on failure (which the user should see + retry).
  const version =
    phase.kind === "available" ||
    phase.kind === "downloading" ||
    phase.kind === "downloaded" ||
    phase.kind === "download-failed"
      ? phase.result.latest_version
      : null;
  if (version == null || dismissedVersion === version) return null;

  const downloading = phase.kind === "downloading";
  const downloaded = phase.kind === "downloaded";
  const failed = phase.kind === "download-failed";

  return (
    <div className="flex items-center gap-3 border-b border-[var(--color-accent)] bg-[var(--color-accent-soft,var(--color-surface-2))] px-4 py-2 text-sm">
      <Sparkles size={15} className="shrink-0 text-[var(--color-accent)]" />
      <span className="min-w-0 flex-1 truncate">
        <span className="font-medium text-[var(--color-text)]">
          {downloaded
            ? tr("update_toast_downloaded", undefined, "Update downloaded")
            : failed
              ? tr("update_toast_failed", undefined, "Update download failed")
              : tr("update_toast_title", undefined, "Update available")}
        </span>
        <span className="text-[var(--color-muted)]"> — v{version}</span>
        {downloaded && (
          <span className="text-[var(--color-muted)]">
            {" · "}
            {tr(
              "update_toast_downloaded_hint",
              undefined,
              "open Settings to install",
            )}
          </span>
        )}
      </span>
      <button
        type="button"
        onClick={() => navigate("/settings")}
        className="shrink-0 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        {tr("update_toast_details", undefined, "Details")}
      </button>
      {/* Download / Retry — hidden once downloaded (the install step lives in
          Settings). Disabled while a download is in flight. */}
      {!downloaded && (
        <button
          type="button"
          disabled={downloading}
          onClick={() => void download()}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-md border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60"
        >
          {downloading ? (
            <Spinner size={14} />
          ) : (
            <Download size={13} />
          )}
          {downloading
            ? tr("update_toast_downloading", undefined, "Downloading…")
            : failed
              ? tr("update_toast_retry", undefined, "Retry")
              : tr("update_toast_download", undefined, "Download")}
        </button>
      )}
      <button
        type="button"
        aria-label={tr("update_toast_dismiss", undefined, "Dismiss")}
        onClick={() => setDismissedVersion(version)}
        className="shrink-0 rounded p-1 text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        <X size={14} />
      </button>
    </div>
  );
}

export default UpdateToast;
