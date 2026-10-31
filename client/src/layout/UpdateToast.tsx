import { useState } from "react";
import { ExternalLink, Download, RefreshCw, X, Sparkles } from "lucide-react";

import { useUpdateStore } from "../state/update";
import { useTr } from "../state/lang";
import { Spinner } from "../components/Spinner";
import { Modal } from "../components/Modal";
import { MarkdownView } from "../components/MarkdownView";
import { Button } from "../components/Button";
import { openExternalUrl } from "../lib/openExternalUrl";
import { isMobile } from "../lib/platform";
import { apkUpdaterAvailable, progressPercent, useApkUpdateStore } from "../state/apkUpdate";

/** The GitHub page of one release. */
export function releasePageUrl(version: string): string {
  return `https://github.com/phantomptr/ps5upload/releases/tag/v${version.replace(/^v/, "")}`;
}

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
  const [showNotes, setShowNotes] = useState(false);
  const phase = useUpdateStore((s) => s.phase);
  const download = useUpdateStore((s) => s.download);
  const install = useUpdateStore((s) => s.install);
  const installing = useUpdateStore((s) => s.installing);
  const apkStep = useApkUpdateStore((s) => s.step);
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
  const notes = "result" in phase ? phase.result.notes : "";

  const downloading = phase.kind === "downloading";
  const downloaded = phase.kind === "downloaded";
  const failed = phase.kind === "download-failed";
  const canInstall = phase.kind === "downloaded" && !!phase.download.can_install;
  // Android with the installer bridge: one tap downloads and opens Android's dialog.
  const apkInstall =
    isMobile() && "result" in phase && !!phase.result.download_url && apkUpdaterAvailable();
  const apkPct =
    apkInstall && apkStep.kind === "downloading"
      ? progressPercent(apkStep.received, apkStep.total)
      : null;

  return (
    <div className="mx-3 mt-2 flex items-center gap-3 rounded-full border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] px-4 py-2 text-sm md:mx-6">
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
        {downloaded && !canInstall && (
          <span className="text-[var(--color-muted)]">
            {" · "}
            {tr(
              "update_toast_downloaded_hint",
              undefined,
              "open Settings to install",
            )}
          </span>
        )}
        {apkInstall && apkStep.kind === "needs-permission" && (
          <span className="text-[var(--color-muted)]">
            {" · "}
            {tr("update_apk_toast_permission", undefined, "allow installing apps, then come back")}
          </span>
        )}
        {apkInstall && apkStep.kind === "installing" && (
          <span className="text-[var(--color-muted)]">
            {" · "}
            {tr("update_apk_toast_confirm", undefined, "tap Update in Android's dialog")}
          </span>
        )}
      </span>
      <button
        type="button"
        onClick={() => setShowNotes(true)}
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
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60"
        >
          {downloading ? (
            <Spinner size={14} />
          ) : (
            <Download size={13} />
          )}
          {downloading
            ? apkPct != null
              ? tr("update_apk_downloading_pct", { percent: apkPct }, "Downloading… {percent}%")
              : tr("update_toast_downloading", undefined, "Downloading…")
            : failed
              ? tr("update_toast_retry", undefined, "Retry")
              : apkInstall
                ? tr("update_apk_toast_install", undefined, "Install")
                : tr("update_toast_download", undefined, "Download")}
        </button>
      )}
      {canInstall && (
        <button
          type="button"
          disabled={installing}
          onClick={() => void install()}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60"
        >
          {installing ? <Spinner size={14} /> : <RefreshCw size={13} />}
          {tr("update_install_restart", undefined, "Install and restart")}
        </button>
      )}
      <button
        type="button"
        aria-label={tr("update_toast_dismiss", undefined, "Dismiss")}
        onClick={() => setDismissedVersion(version)}
        className="shrink-0 rounded-full p-1.5 text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        <X size={14} />
      </button>
      {showNotes && (
        <Modal
          open
          onClose={() => setShowNotes(false)}
          title={tr("whats_new_in", { version }, `What's new in v${version}`)}
          size="xl"
          footer={
            <Button size="sm" variant="ghost" onClick={() => void openExternalUrl(releasePageUrl(version))}>
              <ExternalLink size={13} />
              {tr("library_fpkg_kstuff_tip_repo", undefined, "View on GitHub")}
            </Button>
          }
        >
          <div className="max-h-[60vh] overflow-auto p-4 text-sm">
            {notes.trim() ? (
              <MarkdownView source={notes} />
            ) : (
              <p className="text-[var(--color-muted)]">v{version}</p>
            )}
          </div>
        </Modal>
      )}
    </div>
  );
}

export default UpdateToast;
