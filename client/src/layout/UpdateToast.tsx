import { useEffect, useState } from "react";
import { ExternalLink, Download, RefreshCw, X, Sparkles } from "lucide-react";

import { useUpdateStore } from "../state/update";
import { getAppVersion } from "../lib/appVersion";
import { loadBundledDoc } from "../lib/bundledDoc";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { isTauriEnv } from "../lib/tauriEnv";
import {
  HISTORY_KEYS,
  LAST_SEEN_VERSION_KEY,
  changelogSection,
  shouldShowWhatsNew,
} from "../lib/whatsNew";
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

/** A release's notes in a dialog, with a link to its GitHub page: the banner's Details and
 *  the "what's new" shown once after an update. */
export function ReleaseNotesDialog({
  version,
  notes,
  onClose,
}: {
  version: string;
  notes: string;
  onClose: () => void;
}) {
  const tr = useTr();
  return (
    <Modal
      open
      onClose={onClose}
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
  );
}

/** The first launch of a new version shows its notes once (from the bundled CHANGELOG.md, so
 *  it needs no network). Desktop and phone apps; the browser page updates with the engine. */
export function WhatsNewOnLaunch() {
  const [shown, setShown] = useState<{ version: string; notes: string } | null>(null);
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
        const notes = changelogSection((await loadBundledDoc("changelog")) ?? "", version);
        if (notes && !cancelled) setShown({ version, notes });
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
    <ReleaseNotesDialog version={shown.version} notes={shown.notes} onClose={() => setShown(null)} />
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
  const install = useUpdateStore((s) => s.install);
  const installing = useUpdateStore((s) => s.installing);
  const skippedVersion = useUpdateStore((s) => s.skippedVersion);
  const skipVersion = useUpdateStore((s) => s.skipVersion);
  const installOnQuit = useUpdateStore((s) => s.installOnQuitArmed);
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
  // Skipped (remembered across launches): only Settings still shows it.
  if (version === skippedVersion) return null;
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
        {downloaded && installOnQuit && (
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
    </div>
  );
}

export default UpdateToast;
