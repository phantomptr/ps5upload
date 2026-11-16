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
