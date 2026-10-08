// A game image built on this computer → an Upload queue item that sends it to the PS5, into the
// folder ShadowMount+ watches unless the user picked another. Shared by Convert's "Upload to
// PS5" and Upload's "send a game folder as an image".

import type { AddQueueItem } from "../state/uploadQueue";
import { consoleAddr, hostOf } from "./addr";
import { resolveUploadDest, basename } from "./uploadDest";

export interface ImageUploadPlan {
  host: string;
  /** Drive on the PS5; `/data` when not chosen. */
  volume: string | null;
  /** Folder on that drive; `homebrew` (what ShadowMount+ scans) by default. */
  subpath: string;
  /** Delete the image from this computer once it is on the PS5. */
  deleteAfter: boolean;
}

export const DEFAULT_IMAGE_SUBPATH = "homebrew";

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
