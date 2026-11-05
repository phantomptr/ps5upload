// A game image built on this computer → an Upload queue item that sends it to the PS5, into the
// folder ShadowMount+ watches unless the user picked another. Shared by Convert's "Upload to
// PS5" and Upload's "send a game folder as an image".

import { fetchVolumes, type Volume } from "../api/ps5";
import type { AddQueueItem } from "../state/uploadQueue";
import { consoleAddr, hostOf } from "./addr";
import { isInternalVolume, isPackageDrive, pkgStorageFor } from "./pkgStorage";
import { resolveUploadDest, basename } from "./uploadDest";

export interface ImageUploadPlan {
  host: string;
  /** Drive on the PS5; none chosen: this console's drive for packages (`imageDrive`). */
  volume: string | null;
  /** Folder on that drive; `homebrew` (what ShadowMount+ scans) by default. */
  subpath: string;
  /** Delete the image from this computer once it is on the PS5. */
  deleteAfter: boolean;
}

export const DEFAULT_IMAGE_SUBPATH = "homebrew";

const trimSlash = (p: string) => p.replace(/\/+$/, "") || "/";

export interface ImageDrive {
  /** The drive to use. */
  volume: string;
  /** The drive wanted (picked, or set for packages in Volumes) cannot be used right now. */
  fellBack: boolean;
}

/** Where a game for `host` goes on the console, with the drive list as known (null: unknown).
 *
 *  A drive picked here is used; none picked means the drive set under Volumes › Use for packages
 *  for this console (#673). A drive that is not there or cannot be written falls back as a package
 *  does (lib/pkgStorage.ts), and so does an unreadable list when nothing was picked: internal
 *  storage rather than a failure, as a path under /mnt that is not mounted must never be written. */
export function imageDrive(host: string, chosen: string | null, volumes: Volume[] | null): ImageDrive {
  if (chosen) {
    if (isInternalVolume(chosen) || !volumes) return { volume: chosen, fellBack: false };
    const want = trimSlash(chosen);
    if (volumes.some((v) => trimSlash(v.path) === want && isPackageDrive(v))) return { volume: want, fellBack: false };
  }
  const forPackages = pkgStorageFor(host, volumes);
  return { volume: forPackages.volume ?? "/data", fellBack: !!chosen || forPackages.fellBack };
}

/** `imageDrive` with the console's drive list as it is now. */
export async function imageDriveNow(host: string, chosen: string | null): Promise<ImageDrive> {
  if (chosen && isInternalVolume(chosen)) return { volume: chosen, fellBack: false };
  const volumes = await fetchVolumes(consoleAddr(hostOf(host))).catch(() => null);
  return imageDrive(host, chosen, volumes);
}

/** The drives a game can be sent to, for a choice. */
export function imageDriveChoices(volumes: Volume[] | null): Volume[] {
  return (volumes ?? []).filter(isPackageDrive);
}

/** The plan as it will run: its drive decided for its console. */
export async function resolveImagePlan(plan: ImageUploadPlan): Promise<ImageUploadPlan> {
  const { volume } = await imageDriveNow(plan.host, plan.volume);
  return { ...plan, volume };
}

export function imageUploadItem(
  imagePath: string,
  plan: ImageUploadPlan,
  bytes = 0,
): AddQueueItem {
  const { dest } = resolveUploadDest(plan.volume, plan.subpath, imagePath);
  return {
    sourceKind: "image",
    sourcePath: imagePath,
    displayName: basename(imagePath),
    resolvedDest: dest,
    addr: consoleAddr(hostOf(plan.host)),
    strategy: "overwrite",
    reconcileMode: "fast",
    excludes: [],
    rarPassword: null,
    mountAfterUpload: false,
    mountReadOnly: true,
    registerAfterUpload: false,
    estimatedBytes: bytes,
    deleteSourceAfterUpload: plan.deleteAfter,
  };
}
