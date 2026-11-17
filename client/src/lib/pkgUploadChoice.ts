// What Upload does with a .pkg: install it (upload to the package library, then install), or copy
// it as a plain file to the destination folder, for a mounter that reads packages where they sit
// (Kraken Mount). The last choice is remembered, so the next package takes it too.

import type { KrakenStatus } from "../api/ps5";
import { driveOf, krakenScanRoot } from "./mounter";
import { isInstallPackagePath } from "./pkgDropDedupe";
import { safeGetItem, safeSetItem } from "./safeStorage";

export type PkgChoice = "install" | "copy";

export const PKG_CHOICE_KEY = "ps5upload.pkg_upload_choice";

/** The remembered choice; "install" when none was made. */
export function loadPkgChoice(): PkgChoice {
  return safeGetItem(PKG_CHOICE_KEY) === "copy" ? "copy" : "install";
}

export function savePkgChoice(choice: PkgChoice): void {
  safeSetItem(PKG_CHOICE_KEY, choice);
}

/** The choice a picked source stands for: a package path picked as a "pkg" source installs, the
 *  same path picked as a plain file is copied. Null for anything that is not a package. */
export function pkgChoiceOf(
  source: { kind: string; path: string } | null | undefined,
): PkgChoice | null {
  if (!source || !isInstallPackagePath(source.path)) return null;
  if (source.kind === "pkg") return "install";
  if (source.kind === "file") return "copy";
  return null;
}

/** Where a copied package should go for Kraken Mount to find it, as the destination picker's
 *  volume and folder: its scan root on the drive already chosen, else the first root on a drive
 *  that is connected. Null when Kraken Mount's roots are unknown or none is on a connected drive.
 *  It scans one level deep, and a copied file lands straight in the folder, so this is enough. */
export function krakenCopyDestination(
  volume: string | null,
  kraken: KrakenStatus | null | undefined,
  volumes: readonly string[],
): { volume: string; subpath: string } | null {
  if (!kraken) return null;
  const drive = driveOf(volume ?? "/data") ?? "/data";
  const onDrive = krakenScanRoot(drive, kraken);
  const root =
    onDrive ??
    (kraken.scan_roots ?? []).find((r) => {
      const d = driveOf(r);
      return !!d && volumes.includes(d);
    }) ??
    null;
  if (!root) return null;
  const d = driveOf(root);
  if (!d) return null;
  return { volume: d, subpath: root.slice(d.length).replace(/^\/+|\/+$/g, "") };
}

type Dest = { volume: string | null; subpath: string };
/** The destination a Kraken Mount prefill replaced, and on which console. */
export type KrakenReplaced = Dest & { host: string };

/**
 * One step of the Kraken Mount prefill on the Upload screen. While a package is copied as a file
 * and the user has not chosen a destination themselves, the destination becomes Kraken Mount's
 * folder (`target`); the one it replaced is kept and put back once no copied package remains
 * (unless the user picked one since). `set`: the destination to set now, if any. Pure.
 */
export function krakenPrefillStep(s: {
  host: string;
  copying: boolean;
  userChose: boolean;
  current: Dest;
  target: { volume: string; subpath: string } | null;
  replaced: KrakenReplaced | null;
}): { set: Dest | null; replaced: KrakenReplaced | null } {
  const { host, current, target, replaced } = s;
  if (!s.copying) {
    const back = !!replaced && replaced.host === host && !s.userChose;
    return {
      set: back ? { volume: replaced.volume, subpath: replaced.subpath } : null,
      replaced: null,
    };
  }
  if (s.userChose || !target) return { set: null, replaced };
  const trim = (p: string) => p.replace(/^\/+|\/+$/g, "");
  if (target.volume === (current.volume ?? "/data") && target.subpath === trim(current.subpath))
    return { set: null, replaced };
  return {
    set: target,
    replaced: replaced?.host === host ? replaced : { host, ...current },
  };
}
