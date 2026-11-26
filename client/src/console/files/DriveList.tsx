import { Disc, Eject, HardDrive, History, Network, Plus, RotateCw, Star, TextCursorInput, Usb } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { isDrive, volumeLabel, type Volume } from "../home/Home";
import { t } from "../strings";
import { ActionsButton } from "../ui/ActionsButton";
import { baseName, volumeOf } from "./move";
import type { FilesPrefs } from "./prefs";
import { shareAddress, type SmbShare } from "./smb";

const isUsb = (v: Volume) => /^\/mnt\/usb\d+$/.test(v.path) || v.mount_from.startsWith("/dev/da");

/** A USB stick the helper can eject (USB_EJECT takes /mnt/usb0 to /mnt/usb7). */
export const canEject = (v: Volume) => /^\/mnt\/usb[0-7]$/.test(v.path);

/** The console's own volumes: never unmounted from here. */
export function isSystemVolume(path: string): boolean {
  return (
    path === "/" ||
    /^\/(data|user)(\/|$)/.test(path) ||
    /^\/(system|preinst|dev)/.test(path) ||
    /^\/mnt\/(usb|ext)\d+$/.test(path)
  );
}

/** A disk image PS5Upload mounted (the helper names its source, or it sits under /mnt/ps5upload). */
/**
 * How much new data is likely to fit on internal storage, where the PS5 holds back about a fifth
 * more as data is written (engine volumes.rs likely_fits_bytes, said the same in the app's Volumes);
 * null elsewhere, or when it is all of the free space. For telling, never for refusing.
 */
export function likelyFitsBytes(v: Volume): number | null {
  if (v.path !== "/data") return null;
  const room = Math.max(0, Math.min(v.free_bytes, v.allocatable_bytes ?? v.free_bytes));
  const fits = Math.floor(Math.max(0, room - 1073741824) / 6) * 5;
  return fits < v.free_bytes ? fits : null;
}

export function isImage(v: Volume): boolean {
  return !isDrive(v) && (!!v.source_image || v.path.startsWith("/mnt/ps5upload/")) && !isSystemVolume(v.path);
}

/** A pinned or recent folder's name: its drive's name at a drive's top, else its last folder. */
export function placeLabel(path: string, volumes: Volume[]): string {
  const v = volumeOf(path, volumes);
  if (v && v.path.replace(/\/+$/, "") === path && isDrive(v)) return volumeLabel(v);
  return baseName(path) || "/";
}

function Places({
  kind,
  paths,
  volumes,
  onOpen,
  onActions,
}: {
  kind: "pinned" | "recent";
  paths: string[];
  volumes: Volume[];
  onOpen: (path: string) => void;
  onActions: (path: string) => void;
}) {
  if (!paths.length) return null;
  const Icon = kind === "pinned" ? Star : History;
  const title = t(kind === "pinned" ? "cw_files_pinned" : "cw_files_recent");
  return (
    <section className="cw-places">
      <h2 className="cw-h2">{title}</h2>
      <ul className="cw-files" data-nav-list aria-label={title}>
        {paths.map((p) => (
          <li key={p} className="cw-file" data-nav-item data-place={p} data-place-kind={kind}>
            <button type="button" className="cw-focusable cw-file-main" data-place-main="" onClick={() => onOpen(p)}>
              <Icon className={`cw-file-icon${kind === "pinned" ? " is-pinned" : ""}`} aria-hidden />
              <span className="cw-file-text">
                <span className="cw-file-name">{placeLabel(p, volumes)}</span>
                <span className="cw-dim cw-mono cw-file-sub">{p}</span>
              </span>
            </button>
            <ActionsButton onPress={() => onActions(p)} />
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * Network shares saved on the console: one row each (✕ opens it, "⋯" offers
 * Open, Edit and Remove) and "Add network share". Hidden on a helper without
 * them (shares null).
 */
function Shares({
  shares,
  onOpen,
  onActions,
  onAdd,
}: {
  shares: SmbShare[];
  onOpen: (path: string) => void;
  onActions: (id: string) => void;
  onAdd?: () => void;
}) {
  const title = t("cw_files_smb_title");
  return (
    <section className="cw-places" data-section="shares">
      <h2 className="cw-h2">{title}</h2>
      {shares.length ? (
        <ul className="cw-files" data-nav-list aria-label={title}>
          {shares.map((s) => (
            <li key={s.id} className="cw-file" data-nav-item data-share={s.id}>
              <button type="button" className="cw-focusable cw-file-main" data-share-open={s.id} onClick={() => onOpen(s.path)}>
                <Network className="cw-file-icon" aria-hidden />
                <span className="cw-file-text">
                  <span className="cw-file-name">
                    {s.name}
                    {!s.user ? <span className="cw-tag">{t("cw_files_smb_guest")}</span> : null}
                  </span>
                  <span className="cw-dim cw-mono cw-file-sub">{shareAddress(s)}</span>
                </span>
              </button>
              <ActionsButton onPress={() => onActions(s.id)} />
            </li>
          ))}
        </ul>
      ) : (
        <p className="cw-dim">{t("cw_files_smb_none")}</p>
      )}
      {onAdd ? (
        <div className="cw-toolbar">
          <button type="button" className="cw-focusable cw-button" data-action="smb-add" onClick={onAdd}>
            <Plus aria-hidden />
            <span>{t("cw_files_smb_add")}</span>
          </button>
        </div>
      ) : null}
    </section>
  );
}

/**
 * Files' first level: Pinned folders, one row per drive (internal, extended/M.2,
 * USB) with its free space and, for a USB drive, Eject; images PS5Upload mounted,
 * with Unmount; network shares; then Recent folders. ✕ opens; "⋯" (or △) on a
 * pinned or recent folder offers Open and Pin / Unpin.
 */
export function DriveList({
  volumes,
  error,
  prefs,
  onOpen,
  onRetry,
  onEject,
  onUnmount,
  onPlaceActions,
  onGoTo,
  unmountHeld,
  shares,
  onShareActions,
  onAddShare,
}: {
  volumes: Volume[] | null;
  error: boolean;
  /** Pinned and recent folders; null hides both lists (not read yet, or an older helper). */
  prefs?: FilesPrefs | null;
  onOpen: (path: string) => void;
  /** Read the drives again. */
  onRetry?: () => void;
  onEject?: (path: string) => void;
  onUnmount?: (path: string) => void;
  onPlaceActions?: (path: string) => void;
  onGoTo?: () => void;
  /** Why Unmount is held now (a game is running), shown in its place. */
  unmountHeld?: string | null;
  /** Saved network shares; null hides the section (not read yet, or a helper without them). */
  shares?: SmbShare[] | null;
  onShareActions?: (id: string) => void;
  onAddShare?: () => void;
}) {
  const all = volumes ?? [];
  const drives = all.filter(isDrive);
  const images = all.filter(isImage);
  const placeActions = onPlaceActions ?? (() => undefined);
  const goTo = onGoTo ? (
    <div className="cw-toolbar">
      <button type="button" className="cw-focusable cw-button" data-action="goto" onClick={onGoTo}>
        <TextCursorInput aria-hidden />
        <span>{t("cw_files_goto")}</span>
      </button>
    </div>
  ) : null;
  const pinned = prefs ? (
    <Places kind="pinned" paths={prefs.favorites} volumes={all} onOpen={onOpen} onActions={placeActions} />
  ) : null;
  const recent = prefs ? (
    <Places kind="recent" paths={prefs.recent} volumes={all} onOpen={onOpen} onActions={placeActions} />
  ) : null;

  let driveRows;
  if (!drives.length) {
    driveRows = !error ? (
      <p className="cw-dim">{t("cw_home_loading")}</p>
    ) : (
      <div className="cw-msg is-error" role="alert">
        {t("cw_home_err_load_storage")}
        {onRetry ? (
          <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="retry" onClick={onRetry}>
            <RotateCw aria-hidden />
            <span>{t("cw_files_retry")}</span>
          </button>
        ) : null}
      </div>
    );
  } else {
    driveRows = (
      <ul className="cw-files" data-nav-list aria-label={t("cw_files_drives")}>
        {drives.map((v) => {
          const Icon = isUsb(v) ? Usb : HardDrive;
          const used = v.total_bytes > 0 ? Math.max(0, Math.min(100, 100 - (v.free_bytes / v.total_bytes) * 100)) : 0;
          const eject = onEject && canEject(v);
          const fits = likelyFitsBytes(v);
          return (
            <li key={v.path} className={`cw-file${eject ? "" : " is-plain"}`} data-nav-item data-volume={v.path}>
              <button
                type="button"
                className="cw-focusable cw-file-main"
                data-drive={v.path}
                onClick={() => onOpen(v.path)}
              >
                <Icon className="cw-file-icon" aria-hidden />
                <span className="cw-file-text">
                  <span className="cw-file-name">
                    {volumeLabel(v)}
                    {!v.writable ? <span className="cw-tag">{t("cw_home_vol_readonly")}</span> : null}
                  </span>
                  <span className="cw-dim cw-mono cw-file-sub">{v.path}</span>
                  {fits != null ? <span className="cw-dim">{t("cw_files_likely_fits", { safe: formatBytes(fits) })}</span> : null}
                </span>
                <span className="cw-file-meta">
                  <span className="cw-bar" aria-hidden>
                    <span className={`cw-bar-fill${used >= 90 ? " is-warn" : ""}`} style={{ width: `${used}%` }} />
                  </span>
                  {t("cw_home_free_of", { free: formatBytes(v.free_bytes), total: formatBytes(v.total_bytes) })}
                </span>
              </button>
              {eject ? (
                <button
                  type="button"
                  className="cw-focusable cw-button cw-row-action"
                  data-eject={v.path}
                  onClick={() => onEject(v.path)}
                >
                  <Eject aria-hidden />
                  <span>{t("cw_files_eject")}</span>
                </button>
              ) : null}
            </li>
          );
        })}
      </ul>
    );
  }

  return (
    <>
      {goTo}
      {pinned}
      <section className="cw-places">
        {prefs && (prefs.favorites.length || prefs.recent.length) ? <h2 className="cw-h2">{t("cw_files_drives")}</h2> : null}
        {driveRows}
      </section>
      {images.length ? (
        <section className="cw-places">
          <h2 className="cw-h2">{t("cw_files_images")}</h2>
          <ul className="cw-files" data-nav-list aria-label={t("cw_files_images")}>
            {images.map((v) => (
              <li key={v.path} className={`cw-file${onUnmount ? "" : " is-plain"}`} data-nav-item data-volume={v.path}>
                <button
                  type="button"
                  className="cw-focusable cw-file-main"
                  data-drive={v.path}
                  onClick={() => onOpen(v.path)}
                >
                  <Disc className="cw-file-icon" aria-hidden />
                  <span className="cw-file-text">
                    <span className="cw-file-name">{baseName(v.source_image || v.path)}</span>
                    <span className="cw-dim cw-mono">{v.path}</span>
                  </span>
                  <span className="cw-file-meta cw-dim">{formatBytes(v.total_bytes)}</span>
                </button>
                {onUnmount && unmountHeld ? (
                  <span className="cw-dim cw-row-action" data-unmount-held={v.path}>
                    {unmountHeld}
                  </span>
                ) : null}
                {onUnmount && !unmountHeld ? (
                  <button
                    type="button"
                    className="cw-focusable cw-button cw-row-action"
                    data-unmount={v.path}
                    onClick={() => onUnmount(v.path)}
                  >
                    <Eject aria-hidden />
                    <span>{t("cw_files_unmount")}</span>
                  </button>
                ) : null}
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      {shares ? <Shares shares={shares} onOpen={onOpen} onActions={onShareActions ?? (() => undefined)} onAdd={onAddShare} /> : null}
      {recent}
    </>
  );
}
