// Safely remove a USB drive (/mnt/usbN) — shared by Volumes and the File system drive bar (#496).
//
// The helper unmounts without force first. The PS5's own services usually keep a USB drive open,
// so that is refused on every console tried; the user is then asked whether to eject anyway,
// which flushes the drive first. The engine refuses while a job of ours runs on that console, and
// a forced eject while a game runs: those are real uses, which a forced unmount would cut off.
import { useState, type ReactNode } from "react";

import { usbEject } from "../api/ps5";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm, type ConfirmOptions } from "../components/ConfirmDialog";
import { useTr, type Translator } from "../state/lang";
import { pushNotification } from "../state/notifications";
import { withConsolePrefix } from "../state/roster";
import { useStaleHostGuard } from "./staleHostGuard";

/** A USB stick the helper can eject (USB_EJECT takes /mnt/usb0 to /mnt/usb7). */
export const canEjectUsb = (path: string) => /^\/mnt\/usb[0-7]$/.test(path);

export interface UsbEjectDeps {
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  eject: (host: string, mountPoint: string, force: boolean) => Promise<unknown>;
  /** The user switched console since the click: drop everything. */
  isStale: () => boolean;
  /** The eject request is in flight (true) or over (false). */
  onBusy: (busy: boolean) => void;
}

export type UsbEjectOutcome =
  | { kind: "ejected" }
  | { kind: "cancelled" }
  | { kind: "failed"; message: string };

export async function usbEjectFlow(
  host: string,
  mountPoint: string,
  d: UsbEjectDeps,
  tr: Translator,
): Promise<UsbEjectOutcome> {
  const failed = (e: unknown): UsbEjectOutcome => {
    const raw = e instanceof Error ? e.message : String(e);
    if (/usb_eject_refused_jobs/.test(raw)) {
      return {
        kind: "failed",
        message: tr(
          "volumes_eject_refused_jobs",
          { path: mountPoint },
          `Can't eject ${mountPoint} yet: an upload, file operation or install is running on this PS5. Wait for it to finish or stop it.`,
        ),
      };
    }
    if (/usb_eject_refused_game/.test(raw)) {
      return {
        kind: "failed",
        message: tr(
          "volumes_eject_refused_game",
          { path: mountPoint },
          `Can't eject ${mountPoint} while a game is running: it may be reading from this drive. Close the game, then try again.`,
        ),
      };
    }
    return {
      kind: "failed",
      message: tr("volumes_eject_failed", { path: mountPoint, error: raw }, `Couldn't eject ${mountPoint}: ${raw}`),
    };
  };
  const ok = await d.confirm({
    title: tr("volumes_eject_confirm_title", { path: mountPoint }, `Eject ${mountPoint}?`),
    message: tr(
      "volumes_eject_confirm_body",
      undefined,
      "The drive is unmounted so you can unplug it. Close any game or app on the PS5 that uses it first; the PS5 keeps a drive that is in use.",
    ),
    confirmLabel: tr("volumes_eject", undefined, "Eject"),
  });
  if (!ok || d.isStale()) return { kind: "cancelled" };
  d.onBusy(true);
  try {
    await d.eject(host, mountPoint, false);
    return { kind: "ejected" };
  } catch (e) {
    const raw = e instanceof Error ? e.message : String(e);
    if (!/usb_eject_busy/.test(raw)) return failed(e);
  } finally {
    d.onBusy(false);
  }
  const force = await d.confirm({
    title: tr("volumes_eject_force_title", { path: mountPoint }, `${mountPoint} is still in use — eject anyway?`),
    message: tr(
      "volumes_eject_force_body_v2",
      undefined,
      "The PS5 holds the drive open; its own services usually do. No game is running and this app is not using it, so ejecting anyway should be safe. The drive is flushed first. An app on the PS5 that is using it would be cut off.",
    ),
    confirmLabel: tr("volumes_eject_force", undefined, "Eject anyway"),
    destructive: true,
  });
  if (!force || d.isStale()) return { kind: "cancelled" };
  d.onBusy(true);
  try {
    await d.eject(host, mountPoint, true);
    return { kind: "ejected" };
  } catch (e) {
    return failed(e);
  } finally {
    d.onBusy(false);
  }
}

export function useUsbEject(opts: {
  /** After a successful eject, with the console it was ejected from. */
  onEjected: (host: string, mountPoint: string) => void | Promise<void>;
  onError: (message: string) => void;
}): {
  eject: (mountPoint: string) => Promise<void>;
  ejectingPath: string | null;
  dialog: ReactNode;
} {
  const tr = useTr();
  const guard = useStaleHostGuard();
  const [ejectingPath, setEjectingPath] = useState<string | null>(null);
  // Native window.confirm() is a no-op in Tauri's webview; use the in-tree modal.
  const { confirm, dialog } = useConfirm();

  const eject = async (mountPoint: string) => {
    const probe = guard.capture();
    if (!probe.host?.trim()) return;
    const out = await usbEjectFlow(
      probe.host,
      mountPoint,
      {
        confirm,
        eject: (host, mp, force) => usbEject(host, mp, force),
        isStale: probe.isStale,
        onBusy: (busy) => setEjectingPath(busy ? mountPoint : null),
      },
      tr,
    );
    if (probe.isStale()) return;
    if (out.kind === "failed") opts.onError(out.message);
    if (out.kind !== "ejected") return;
    pushNotification(
      "success",
      withConsolePrefix(
        probe.host,
        tr("volumes_eject_done", { path: mountPoint }, `${mountPoint} ejected — safe to unplug`),
      ),
    );
    await opts.onEjected(probe.host, mountPoint);
  };

  return { eject, ejectingPath, dialog };
}
