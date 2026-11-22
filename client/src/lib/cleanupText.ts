/**
 * Console clean-up wording, shared by the PC app's Clean up screen and the console page: both
 * read the same `cw_cleanup_*` keys (the console page only carries `cw_*` keys).
 */
import type { CleanupCategory, CleanupItem } from "../api/cleanup";
import { driveParts } from "./cleanupModel";
import { formatBytes } from "./format";

export type CleanupT = (key: string, vars?: Record<string, string | number>) => string;

/** Kraken Mount's wording (#894) is not among the console page's `cw_*` keys yet: there `t` answers
 *  the key itself, so the English text stands in. */
const KRAKEN_EN: Record<string, string> = {
  cleanup_kept_kraken: "Used by Kraken Mount, kept",
  cleanup_reason_kraken: "used by Kraken Mount",
  cleanup_err_kraken: "Nothing was removed: Kraken Mount uses everything you picked, and deleting it would remove the game.",
};

function krakenText(t: CleanupT, key: keyof typeof KRAKEN_EN & string): string {
  const s = t(key);
  return s && s !== key ? s : KRAKEN_EN[key];
}

export function driveName(t: CleanupT, drive: string): string {
  const d = driveParts(drive);
  if (d.kind === "internal") return t("cw_cleanup_drive_internal");
  if (d.kind === "ext") return t("cw_cleanup_drive_ext", { n: d.n + 1 });
  if (d.kind === "usb") return t("cw_cleanup_drive_usb", { n: d.n + 1 });
  return drive;
}

export function categoryTitle(t: CleanupT, cat: CleanupCategory): string {
  switch (cat) {
    case "A":
      return t("cw_cleanup_cat_a");
    case "B":
      return t("cw_cleanup_cat_b");
    case "C":
      return t("cw_cleanup_cat_c");
    case "D":
      return t("cw_cleanup_cat_d");
    case "E":
      return t("cw_cleanup_cat_e");
    case "F":
      return t("cw_cleanup_cat_f");
    default:
      return t("cw_cleanup_cat_g");
  }
}

export function categoryHint(t: CleanupT, cat: CleanupCategory): string {
  switch (cat) {
    case "A":
      return t("cw_cleanup_cat_a_hint");
    case "B":
      return t("cw_cleanup_cat_b_hint");
    case "C":
      return t("cw_cleanup_cat_c_hint");
    case "D":
      return t("cw_cleanup_cat_d_hint");
    case "E":
      return t("cw_cleanup_cat_e_hint");
    case "F":
      return t("cw_cleanup_cat_f_hint");
    default:
      return t("cw_cleanup_cat_g_hint");
  }
}

export function stateText(t: CleanupT, item: CleanupItem): string {
  switch (item.state) {
    case "update_installed":
      return t("cw_cleanup_state_update_installed");
    case "game_gone":
      return t("cw_cleanup_state_game_gone");
    case "installed_same":
      return t("cw_cleanup_state_installed_same");
    case "installed_newer":
      return t("cw_cleanup_state_installed_newer");
    case "not_installed_yet":
      return t("cw_cleanup_state_not_installed_yet");
    case "game_installed":
      return t("cw_cleanup_state_game_installed");
    case "dlc_installed":
      return t("cw_cleanup_state_dlc_installed");
    case "dlc_not_installed":
      return t("cw_cleanup_state_dlc_not_installed");
    case "not_installed":
      return t("cw_cleanup_state_not_installed");
    case "partial":
      return t("cw_cleanup_state_partial");
    case "unreadable":
      return t("cw_cleanup_state_unreadable");
    case "temp":
      return t("cw_cleanup_state_temp");
    case "recent":
      return t("cw_cleanup_state_recent");
    case "appmeta_orphan":
      return t("cw_cleanup_state_appmeta_orphan");
    case "playgo_orphan":
      return t("cw_cleanup_state_playgo_orphan");
    case "duplicate":
      return t("cw_cleanup_state_duplicate");
    case "system_update_file":
      return t("cw_cleanup_state_system_update_file");
    case "large_file":
      return t("cw_cleanup_state_large_file");
    case "addcont_orphan":
      return t("cw_cleanup_state_addcont_orphan");
    default:
      return item.state;
  }
}

/** The details under an item's name: why it is listed, versions, where the game is. */
export function itemDetails(t: CleanupT, item: CleanupItem): string {
  const bits: string[] = item.kept === "kraken" ? [krakenText(t, "cleanup_kept_kraken")] : [];
  bits.push(stateText(t, item));
  if (item.version) bits.push(t("cw_cleanup_version", { version: item.version }));
  if (item.installed_version && item.cat === "C")
    bits.push(t("cw_cleanup_installed_version", { version: item.installed_version }));
  if (item.installed_version && item.cat === "A")
    bits.push(t("cw_cleanup_base_version", { version: item.installed_version }));
  if (item.cat === "A" && item.game_drive)
    bits.push(
      t(item.game_backup ? "cw_cleanup_game_backup_on" : "cw_cleanup_game_on", { drive: driveName(t, item.game_drive) }),
    );
  return bits.join(" · ");
}

export function reasonText(t: CleanupT, reason: string, code?: string): string {
  switch (reason) {
    case "changed":
      return t("cw_cleanup_reason_changed");
    case "in_use":
      return t("cw_cleanup_reason_in_use");
    case "game_running":
      return t("cw_cleanup_reason_game_running");
    case "install_running":
      return t("cw_cleanup_reason_install_running");
    case "game_installed":
      return t("cw_cleanup_reason_game_installed");
    case "cancelled":
      return t("cw_cleanup_reason_cancelled");
    case "still_present":
      return t("cw_cleanup_reason_still_present");
    case "sony_refused":
      return t("cw_cleanup_reason_sony_refused", { code: code ?? "" });
    case "delete_failed":
      return t("cw_cleanup_reason_delete_failed", { code: code ?? "" });
    case "not_supported":
      return t("cw_cleanup_reason_not_supported");
    case "kraken":
      return krakenText(t, "cleanup_reason_kraken");
    default:
      return t("cw_cleanup_reason_not_allowed");
  }
}

export function phaseText(t: CleanupT, phase: string): string {
  switch (phase) {
    case "packages":
      return t("cw_cleanup_phase_packages");
    case "temp":
      return t("cw_cleanup_phase_temp");
    case "downloads":
      return t("cw_cleanup_phase_downloads");
    case "leftovers":
      return t("cw_cleanup_phase_leftovers");
    case "duplicates":
      return t("cw_cleanup_phase_duplicates");
    default:
      return t("cw_cleanup_phase_updates");
  }
}

/** The selection as "312 GiB on internal storage, 8 GiB on extended storage 1". */
export function selectionLine(t: CleanupT, byDrive: { drive: string; bytes: number }[]): string {
  return byDrive
    .map((d) => t("cw_cleanup_size_on", { size: formatBytes(d.bytes), drive: driveName(t, d.drive) }))
    .join(", ");
}

/** A refusal token (busy, install_running, scan_expired, unsupported) as a sentence; null for others. */
export function refusalText(t: CleanupT, reason: string): string | null {
  switch (reason) {
    case "busy":
      return t("cw_cleanup_err_busy");
    case "install_running":
      return t("cw_cleanup_err_install_running");
    case "kraken":
      return krakenText(t, "cleanup_err_kraken");
    case "scan_expired":
      return t("cw_cleanup_err_expired");
    case "unknown_scan":
    case "unknown_apply":
      return t("cw_cleanup_err_restarted");
    case "unsupported":
    case "unavailable":
    case "not_found":
      return t("cw_cleanup_err_unsupported");
    default:
      return null;
  }
}
