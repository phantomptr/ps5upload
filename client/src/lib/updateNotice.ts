// Where the app tells people about its own updates, as a pure model (layout/UpdateBanner,
// Settings, the navigation dots).
//
// A routine update is quiet: a dot on Settings (the sidebar row; the More tab and its Settings row
// on a phone), an "Update available" row at the top of Settings, and the notification bell. A
// banner across the top is kept for the two cases where the app does not work properly until
// the person acts:
//
//   - the PS5 runs a NEWER helper than this app (another device sent it): the fix is updating this
//     app, so it is one line, "Update this app to vX to match your PS5", merged with the update
//     itself (Install and Details act on the update);
//   - an update of this app failed to install.
//
// Never two banners: with both, the more important one shows with a count of the rest.

import { compareVersions } from "./semver";

export type BannerKind = "install-failed" | "helper-newer";

export interface NoticeInput {
  /** This app's version; null until known. */
  appVersion: string | null;
  /** The selected console's helper version, while its helper answers; null otherwise. */
  helperVersion: string | null;
  /** The update waiting to be installed (found, downloading, downloaded, or its download
   *  failed) and not skipped; null when there is none. */
  waitingVersion: string | null;
  /** The last install of `waitingVersion` failed (or Android refused the APK). */
  installFailed: boolean;
  /** Banner ids the person dismissed (`bannerId`). */
  dismissed: readonly string[];
}

export interface UpdateBannerModel {
  kind: BannerKind;
  /** The version the line names: the update's, else the helper's (the version to update to). */
  version: string;
  /** What the Dismiss button remembers: the version pair for a newer helper, the version for a
   *  failed install. */
  id: string;
  /** How many more things need attention (shown as "+N", which opens Settings). */
  more: number;
}

export interface UpdateNotice {
  /** The dot on Settings (and on More on a phone). */
  dot: boolean;
  /** The row at the top of Settings; null when there is nothing to say. */
  row: { version: string; helperNewer: boolean } | null;
  /** The one-line banner; null for a routine update. */
  banner: UpdateBannerModel | null;
}

/** Whether the helper is newer than this app (another device sent it). Unknown or unparsable
 *  versions are not. */
export function helperIsNewer(helperVersion: string | null, appVersion: string | null): boolean {
  if (!helperVersion || !appVersion) return false;
  return compareVersions(helperVersion, appVersion) === 1;
}

/** The pair a newer-helper banner is dismissed for (the same form the older banner stored). */
export function helperPair(helperVersion: string, appVersion: string): string {
  return `${helperVersion}->${appVersion}`;
}

/** What a banner's Dismiss remembers. */
export function bannerId(kind: BannerKind, key: string): string {
  return `${kind}:${key}`;
}

/** The version to name in "Update this app to vX": the waiting update when it reaches the
 *  helper's version (it replaces it as well), else the helper's. */
function targetVersion(helper: string, waiting: string | null): string {
  if (waiting && (compareVersions(waiting, helper) ?? -1) >= 0) return waiting;
  return helper;
}

export function updateNotice(i: NoticeInput): UpdateNotice {
  const helperNewer = helperIsNewer(i.helperVersion, i.appVersion);
  const items: UpdateBannerModel[] = [];
  if (i.installFailed && i.waitingVersion) {
    items.push({
      kind: "install-failed",
      version: i.waitingVersion,
      id: bannerId("install-failed", i.waitingVersion),
      more: 0,
    });
  }
  if (helperNewer && i.helperVersion && i.appVersion) {
    items.push({
      kind: "helper-newer",
      version: targetVersion(i.helperVersion, i.waitingVersion),
      id: bannerId("helper-newer", helperPair(i.helperVersion, i.appVersion)),
      more: 0,
    });
  }
  const open = items.filter((b) => !i.dismissed.includes(b.id));
  const banner = open.length > 0 ? { ...open[0], more: open.length - 1 } : null;
  const rowVersion = i.waitingVersion ?? (helperNewer ? i.helperVersion : null);
  const row = rowVersion ? { version: rowVersion, helperNewer } : null;
  return { dot: row !== null, row, banner };
}
