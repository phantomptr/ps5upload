import { useEffect, useState } from "react";
import { ExternalLink, Download, RefreshCw, X, Sparkles } from "lucide-react";

import { failedInstall, useUpdateStore } from "../state/update";
import { getAppVersion } from "../lib/appVersion";
import { loadBundledDoc } from "../lib/bundledDoc";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { isTauriEnv } from "../lib/tauriEnv";
import {
  CHANGELOG_URL,
  HISTORY_KEYS,
  LAST_SEEN_VERSION_KEY,
  changelogSince,
  changelogSinceMarkdown,
  shouldShowWhatsNew,
} from "../lib/whatsNew";
import { useTr } from "../state/lang";
import { Spinner } from "../components/Spinner";
import { Modal } from "../components/Modal";
import { Button } from "../components/Button";
import { openExternalUrl } from "../lib/openExternalUrl";
import { ReleaseNotesBody } from "./ReleaseNotesBody";
import { isIOS, isMobile } from "../lib/platform";
import { useApkInstallGuard } from "./useApkInstallGuard";
import { apkUpdaterAvailable, progressPercent, useApkUpdateStore } from "../state/apkUpdate";

/** The GitHub page of one release. */
export function releasePageUrl(version: string): string {
  return `https://github.com/phantomptr/ps5upload/releases/tag/v${version.replace(/^v/, "")}`;
}

/** A release's notes in a dialog, with a link to its GitHub page: the banner's Details and
 *  the "what's new" shown once after an update. */
export function ReleaseNotesDialog({
  version,
  notes,
  since,
  more,
  onClose,
}: {
  version: string;
  notes: string;
  /** The notes cover every release since this version (each under its own heading). */
  since?: string;
  /** Older releases were left out: link the full changelog too. */
  more?: boolean;
  onClose: () => void;
}) {
  const tr = useTr();
  return (
    <Modal
      open
      onClose={onClose}
      title={
        since
          ? tr("whats_new_since", { version: since }, `What's new since v${since}`)
          : tr("whats_new_in", { version }, `What's new in v${version}`)
      }
      size="xl"
      footer={
        <>
          {more && (
            <Button size="sm" variant="ghost" onClick={() => void openExternalUrl(CHANGELOG_URL)}>
              <ExternalLink size={13} />
              {tr("whats_new_full_changelog", undefined, "Full changelog on GitHub")}
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => void openExternalUrl(releasePageUrl(version))}>
            <ExternalLink size={13} />
            {tr("library_fpkg_kstuff_tip_repo", undefined, "View on GitHub")}
          </Button>
        </>
      }
    >
      <div className="max-h-[60vh] overflow-auto p-4 text-sm">
        <ReleaseNotesBody version={version} notes={notes} />
      </div>
    </Modal>
  );
}

/** The first launch of a new version shows its notes once (from the bundled CHANGELOG.md, so
 *  it needs no network), and those of every release skipped since the last version seen (frequent
 *  releases: 10.7.1 → 10.7.3 also shows 10.7.2). Desktop and phone apps; the browser page updates
 *  with the engine. */
export function WhatsNewOnLaunch() {
  const [shown, setShown] = useState<{
    version: string;
    notes: string;
    since?: string;
    more: boolean;
  } | null>(null);
  useEffect(() => {
    if (!isTauriEnv()) return;
    let cancelled = false;
    void (async () => {
      try {
        const version = await getAppVersion();
        if (!version || cancelled) return;
        const lastSeen = safeGetItem(LAST_SEEN_VERSION_KEY);
        if (lastSeen === version) return;
        const usedBefore = HISTORY_KEYS.some((k) => safeGetItem(k) != null);
        // Remembered before anything is shown: once means once, even if this launch dies.
        safeSetItem(LAST_SEEN_VERSION_KEY, version);
        if (!shouldShowWhatsNew(lastSeen, version, usedBefore)) return;
        const since = changelogSince((await loadBundledDoc("changelog")) ?? "", lastSeen, version);
        if (since.sections.length === 0 || cancelled) return;
        if (since.sections.length === 1) {
          setShown({ version, notes: since.sections[0].notes, more: false });
        } else {
          setShown({
            version,
            notes: changelogSinceMarkdown(since),
            since: lastSeen ?? undefined,
            more: since.truncated,
          });
        }
      } catch {
        // No version or no changelog: nothing to show.
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);
  if (!shown) return null;
  return (
    <ReleaseNotesDialog
      version={shown.version}
      notes={shown.notes}
      since={shown.since}
      more={shown.more}
      onClose={() => setShown(null)}
    />
  );
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
  const downloadProgress = useUpdateStore((s) => s.downloadProgress);
  const cancelDownload = useUpdateStore((s) => s.cancelDownload);
  const install = useUpdateStore((s) => s.install);
  const installing = useUpdateStore((s) => s.installing);
  const skippedVersion = useUpdateStore((s) => s.skippedVersion);
  const skipVersion = useUpdateStore((s) => s.skipVersion);
  const installOnQuit = useUpdateStore((s) => s.installOnQuitArmed);
  const lastInstall = useUpdateStore((s) => s.lastInstall);
  const retryInstall = useUpdateStore((s) => s.retryInstall);
  const apkStep = useApkUpdateStore((s) => s.step);
  const { confirmInstall, guardDialog } = useApkInstallGuard();
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
  // Skipped (remembered across launches): only Settings still shows it.
  if (version === skippedVersion) return null;
  const notes = "result" in phase ? phase.result.notes : "";

  const downloading = phase.kind === "downloading";
  const downloaded = phase.kind === "downloaded";
  const failed = phase.kind === "download-failed";
  const canInstall = phase.kind === "downloaded" && !!phase.download.can_install;
  // The last install of this version failed (#611): not tried again by itself.
  const installFailed = failedInstall({ lastInstall }, version) != null;
  // Downloaded (often in the background) and armed for the quit: a quiet "ready" line with
  // Restart now, nothing to do otherwise.
  const ready = canInstall && installOnQuit && !installFailed;
  // Android with the installer bridge: one tap downloads and opens Android's dialog.
  const apkInstall =
    isMobile() && "result" in phase && !!phase.result.download_url && apkUpdaterAvailable();
  // Android refused after its dialog (Play Protect, a conflict, no space), or Install unknown
  // apps stayed off. `phase` is back to "available" by then, so this is the only place it shows.
  const apkFailed = apkInstall && apkStep.kind === "failed" && !downloading ? apkStep.message : null;
  const downloadPct =
    apkInstall && apkStep.kind === "downloading"
      ? progressPercent(apkStep.received, apkStep.total)
      : downloadProgress
        ? progressPercent(downloadProgress.received, downloadProgress.total)
        : null;

  return (
    // A phone has no room for this on one line: the text takes its own row, in full.
    <div className="mx-3 mt-2 flex items-center gap-3 rounded-full border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] px-4 py-2 text-sm max-sm:flex-wrap max-sm:gap-y-1 max-sm:rounded-[var(--radius-card)] md:mx-6">
      <Sparkles size={15} className="shrink-0 text-[var(--color-accent)]" />
      <span className="min-w-0 flex-1 truncate max-sm:basis-[calc(100%-2.5rem)] max-sm:whitespace-normal">
        <span className="font-medium text-[var(--color-text)]">
          {ready
            ? tr("update_toast_ready", { version }, "v{version} ready")
            : installFailed
              ? tr("update_install_failed_title", undefined, "Update didn't install")
              : downloaded
                ? tr("update_toast_downloaded", undefined, "Update downloaded")
                : apkFailed
                  ? tr("update_install_failed_title", undefined, "Update didn't install")
                : failed
                  ? tr("update_toast_failed", undefined, "Update download failed")
                  : tr("update_toast_title", undefined, "Update available")}
        </span>
        {!ready && <span className="text-[var(--color-muted)]"> — v{version}</span>}
        {downloaded && installOnQuit && !installFailed && (
          <span className="text-[var(--color-muted)]">
            {" · "}
            {tr("update_toast_installs_on_quit", undefined, "installs when you quit")}
          </span>
        )}
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
        {apkFailed && (
          <span className="text-[var(--color-bad)]" data-testid="update-toast-apk-failed">
            {" · "}
            {apkFailed}
          </span>
        )}
        {/* iPhone: Download only opens the file; nothing installs it from here. */}
        {isIOS() && !downloaded && (
          <span className="text-[var(--color-muted)]">
            {" · "}
            {tr("update_ios_toast_hint", undefined, "install the new .ipa with your sideloading app")}
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
      {installFailed && !downloading && (
        <>
          <button
            type="button"
            disabled={installing}
            onClick={() => void retryInstall()}
            className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60"
          >
            {installing ? <Spinner size={14} /> : <RefreshCw size={13} />}
            {tr("update_try_again", undefined, "Try again")}
          </button>
          <button
            type="button"
            onClick={() => void openExternalUrl(releasePageUrl(version))}
            className="inline-flex shrink-0 items-center gap-1.5 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)] max-sm:hidden"
          >
            <ExternalLink size={13} />
            {tr("update_download_page", undefined, "Download page")}
          </button>
        </>
      )}
      {apkFailed && (
        <button
          type="button"
          onClick={() => void openExternalUrl(releasePageUrl(version))}
          className="inline-flex shrink-0 items-center gap-1.5 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
        >
          <ExternalLink size={13} />
          {tr("update_download_page", undefined, "Download page")}
        </button>
      )}
      {!downloaded && !installFailed && (
        <button
          type="button"
          disabled={downloading}
          onClick={() => void confirmInstall().then((go) => (go ? download() : undefined))}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60"
        >
          {downloading ? (
            <Spinner size={14} />
          ) : (
            <Download size={13} />
          )}
          {downloading
            ? downloadPct != null
              ? tr("update_apk_downloading_pct", { percent: downloadPct }, "Downloading… {percent}%")
              : tr("update_toast_downloading", undefined, "Downloading…")
            : failed
              ? tr("update_toast_retry", undefined, "Retry")
              : apkInstall
                ? tr("update_apk_toast_install", undefined, "Install")
                : tr("update_toast_download", undefined, "Download")}
        </button>
      )}
      {downloading && !isMobile() && (
        <button
          type="button"
          onClick={() => void cancelDownload()}
          className="shrink-0 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
        >
          {tr("cancel", undefined, "Cancel")}
        </button>
      )}
      {canInstall && !installFailed && (
        <button
          type="button"
          disabled={installing}
          onClick={() => void install()}
          className="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60"
        >
          {installing ? <Spinner size={14} /> : <RefreshCw size={13} />}
          {ready
            ? tr("update_restart_now", undefined, "Restart now")
            : tr("update_install_restart", undefined, "Install and restart")}
        </button>
      )}
      {!downloading && !installing && (
        <button
          type="button"
          onClick={skipVersion}
          className="shrink-0 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)] max-sm:hidden"
        >
          {tr("update_skip_version", undefined, "Skip this version")}
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
        <ReleaseNotesDialog version={version} notes={notes} onClose={() => setShowNotes(false)} />
      )}
      {guardDialog}
    </div>
  );
}

export default UpdateToast;
