import { useState } from "react";
import { useNavigate } from "react-router";
import { AlertTriangle, Download, RefreshCw, X } from "lucide-react";

import { Spinner } from "../components/Spinner";
import { isTauriEnv } from "../lib/tauriEnv";
import { isMobile } from "../lib/platform";
import type { UpdateBannerModel } from "../lib/updateNotice";
import { useTr } from "../state/lang";
import { useUpdateStore } from "../state/update";
import { useBannerDismissStore, useUpdateNotice } from "../state/updateNoticeRuntime";
import { ReleaseNotesDialog } from "./UpdateToast";
import { useApkInstallGuard } from "./useApkInstallGuard";
import { useWebUpdateStore, WebUpdateHowTo } from "./WebUpdateToast";

const action =
  "inline-flex shrink-0 items-center gap-1.5 rounded-full border border-[var(--color-accent)] px-2.5 py-1 text-xs font-medium text-[var(--color-accent)] disabled:opacity-60";
const quiet = "shrink-0 rounded px-2 py-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]";

/**
 * The one banner about updates, only when the app needs the person to act (lib/updateNotice):
 * the PS5 runs a newer helper than this app, or an update of this app did not install. A routine
 * update is a dot on Settings and a row there instead.
 *
 * One line at every width (a phone gets the text cut short, never a second row), and never two
 * banners: a second thing shows as "+1", which opens Settings.
 */
export function UpdateBanner() {
  const notice = useUpdateNotice();
  if (!notice.banner) return null;
  return <UpdateBannerView banner={notice.banner} />;
}

export function UpdateBannerView({ banner }: { banner: UpdateBannerModel }) {
  const tr = useTr();
  const navigate = useNavigate();
  const dismiss = useBannerDismissStore((s) => s.dismiss);
  const phase = useUpdateStore((s) => s.phase);
  const installing = useUpdateStore((s) => s.installing);
  const webUpdate = useWebUpdateStore((s) => s.update);
  const { confirmInstall, guardDialog } = useApkInstallGuard();
  const [details, setDetails] = useState(false);
  const tauri = isTauriEnv();
  const failed = banner.kind === "install-failed";
  const downloading = phase.kind === "downloading";

  const text = failed
    ? `${tr("update_install_failed_title", undefined, "Update didn't install")} — v${banner.version}`
    : tauri
      ? tr("update_banner_match_helper", { version: banner.version }, "Update this app to v{version} to match your PS5")
      : tr(
          "update_banner_match_helper_web",
          { version: banner.version },
          "Update PS5Upload where this page comes from to v{version} to match your PS5",
        );

  // Install: the update found for this version, as the Settings row would; without one yet, check
  // now and open Settings, where the result shows.
  const install = async () => {
    const s = useUpdateStore.getState();
    const p = s.phase;
    if (p.kind === "downloaded" && p.download.can_install) {
      await s.install();
    } else if (p.kind === "available" || p.kind === "download-failed") {
      if (await confirmInstall()) await s.download();
    } else if (p.kind !== "downloading") {
      void s.checkNow();
      navigate("/settings");
    }
  };
  const notes = "result" in phase && phase.result.latest_version === banner.version ? phase.result.notes : null;

  return (
    <div
      role="status"
      data-testid="update-banner"
      data-kind={banner.kind}
      title={text}
      className={`mx-3 mt-2 flex h-11 items-center gap-2 overflow-hidden rounded-full border px-3 text-sm md:mx-6 ${
        failed
          ? "border-[color-mix(in_srgb,var(--color-bad)_35%,transparent)] bg-[var(--color-bad-soft)]"
          : "border-[color-mix(in_srgb,var(--color-warn)_35%,transparent)] bg-[var(--color-warn-soft)]"
      }`}
    >
      {failed ? (
        <AlertTriangle size={15} aria-hidden className="shrink-0 text-[var(--color-bad)]" />
      ) : (
        <RefreshCw size={15} aria-hidden className="shrink-0 text-[var(--color-warn)]" />
      )}
      <span className="min-w-0 flex-1 truncate font-medium text-[var(--color-text)]">{text}</span>
      {banner.more > 0 && (
        <button
          type="button"
          onClick={() => navigate("/settings")}
          className="shrink-0 rounded-full bg-[var(--color-surface-3)] px-2 py-0.5 text-xs font-semibold tabular-nums"
          aria-label={tr("update_banner_more", { count: banner.more }, "{count} more in Settings")}
          data-testid="update-banner-more"
        >
          +{banner.more}
        </button>
      )}
      {/* Details: the release notes, or (a failed install, the web UI without notes) Settings,
          where the reason and every choice are. */}
      <button
        type="button"
        className={`${quiet} max-[360px]:hidden`}
        onClick={() => {
          if (failed) navigate("/settings");
          else if (!tauri || notes !== null) setDetails(true);
          else navigate("/settings");
        }}
      >
        {tr("update_toast_details", undefined, "Details")}
      </button>
      {tauri && failed && !isMobile() && (
        <button type="button" className={action} disabled={installing} onClick={() => void useUpdateStore.getState().retryInstall()}>
          {installing ? <Spinner size={14} /> : <RefreshCw size={13} />}
          {tr("update_try_again", undefined, "Try again")}
        </button>
      )}
      {tauri && !failed && (
        <button type="button" className={action} disabled={downloading || installing} onClick={() => void install()}>
          {downloading || installing ? <Spinner size={14} /> : <Download size={13} />}
          {tr("update_apk_toast_install", undefined, "Install")}
        </button>
      )}
      <button
        type="button"
        aria-label={tr("update_toast_dismiss", undefined, "Dismiss")}
        onClick={() => dismiss(banner.id)}
        className="shrink-0 rounded-full p-1.5 text-[var(--color-muted)] hover:text-[var(--color-text)]"
      >
        <X size={14} />
      </button>
      {details &&
        (tauri ? (
          <ReleaseNotesDialog version={banner.version} notes={notes ?? ""} onClose={() => setDetails(false)} />
        ) : (
          <WebUpdateHowTo version={banner.version} update={webUpdate} onClose={() => setDetails(false)} />
        ))}
      {guardDialog}
    </div>
  );
}

export default UpdateBanner;
