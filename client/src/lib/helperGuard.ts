// Before this app sends its helper to a console that already runs one (8.0.0): the send restarts
// the helper for every device, so say who is busy, and refuse to quietly put an older helper over
// a newer one (every device would lose the newer one's features and fixes).

import type { MeshActivityItem, MeshDevice, MeshState } from "../api/mesh";
import type { HelperBusy } from "../api/ps5";
import { deviceName, othersBusy } from "../state/mesh";
import type { Translator } from "../state/lang";
import { compareVersions } from "./semver";

/** What the console reports a restart would stop (STATUS_ACK `busy`, 12.6.15), in plain words
 *  ("an install, remote controls"). */
export function consoleWorkText(tr: Translator, b: HelperBusy): string {
  const parts: string[] = [];
  if (b.uploads > 0) parts.push(tr("helper_work_transfer", undefined, "a transfer"));
  if (b.install) parts.push(tr("helper_work_install", undefined, "an install"));
  if (b.extract) parts.push(tr("helper_work_extract", undefined, "an archive extraction"));
  if (b.convert) parts.push(tr("helper_work_convert", undefined, "a conversion"));
  if (b.dump) parts.push(tr("helper_work_dump", undefined, "a game dump"));
  if (b.input) parts.push(tr("helper_work_input", undefined, "remote controls"));
  if (b.pads > 0) parts.push(tr("helper_work_pads", undefined, "virtual controllers"));
  return parts.join(", ");
}

export interface ReplaceCheck {
  /** The running helper is newer than this app's. */
  downgrade: { running: string; ours: string } | null;
  /** Other devices with work on this console right now. */
  busy: { device: MeshDevice; items: MeshActivityItem[] }[];
}

export function replaceCheck(
  state: MeshState | null,
  runningVersion: string | null,
  appVersion: string | null,
): ReplaceCheck {
  const cmp = runningVersion && appVersion ? compareVersions(runningVersion, appVersion) : null;
  return {
    downgrade: cmp === 1 && runningVersion && appVersion ? { running: runningVersion, ours: appVersion } : null,
    busy: othersBusy(state),
  };
}

/** "MacBook: Upload Astro Bot 42%; Pixel: Install Minecraft" */
export function busyText(busy: ReplaceCheck["busy"]): string {
  return busy
    .map(({ device, items }) => {
      const work = items
        .slice(0, 3)
        .map((a) => [a.kind, a.title, typeof a.pct === "number" ? `${a.pct}%` : ""].filter(Boolean).join(" "));
      return `${deviceName(device)}: ${work.join(", ")}`;
    })
    .join("; ");
}
