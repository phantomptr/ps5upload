// "Send to another console" (#433): what a selection becomes on the other console, decided
// before anything runs.

import { volumeAllocatableBytes, volumeLikelyFitsBytes, type Volume } from "../api/ps5";
import { volumeOf } from "./moveTo";

export interface SendItem {
  path: string;
  name: string;
  /** Bytes; 0 for a folder the listing has not measured. */
  size: number;
  isDir: boolean;
}

export interface ConsoleSendPlan {
  /** Where each item lands on the other console, in order. */
  dests: string[];
  /** Bytes of the files sent (folders not measured count 0). */
  bytes: number;
  /** "unknown" when only unmeasured folders are sent; "no" only when it certainly won't fit. */
  fits: "yes" | "tight" | "no" | "unknown";
  /** Names already in the destination folder: they are replaced. */
  replacing: string[];
}

const join = (dir: string, name: string) => `${dir === "/" ? "" : dir.replace(/\/+$/, "")}/${name}`;

export function planConsoleSend(
  items: SendItem[],
  destDir: string,
  targetVolumes: Volume[],
  existingNames: string[],
): ConsoleSendPlan {
  const dests = items.map((i) => join(destDir, i.name));
  const bytes = items.reduce((n, i) => n + (!i.isDir && i.size > 0 ? i.size : 0), 0);
  const measured = items.some((i) => !i.isDir);
  const vol = volumeOf(destDir, targetVolumes);
  let fits: ConsoleSendPlan["fits"] = measured ? "yes" : "unknown";
  if (measured && vol) {
    if (bytes > volumeAllocatableBytes(vol)) fits = "no";
    else if (bytes > volumeLikelyFitsBytes(vol)) fits = "tight";
  }
  const existing = new Set(existingNames);
  const replacing = items.filter((i) => existing.has(i.name)).map((i) => i.name);
  return { dests, bytes, fits, replacing };
}
