// Put a debug FPKG where Kraken Mount mounts it, instead of installing it.
//
// Kraken Mount (quer3q/kraken-mount 0.2.0) scans its roots one level deep every 10 s and puts
// every PS5 debug package it finds on the home screen without installing it (src/scan.rs:149-176,
// src/main.rs:659). So "mount a package" is: upload it straight into a scan root on the chosen
// drive, then ask it to scan now. Nothing here installs, and the upload is a plain file upload
// (not a package-library item), so no install can follow it by accident.

import { fetchConsoleStat } from "../api/consoleFiles";
import { fsMkdir, isPathMissingError, listVolumes, smpRescan, smpStatus, type SmpStatus } from "../api/ps5";
import { keepBothName } from "../lib/consolePath";
import { mgmtAddr, transferAddr } from "../lib/addr";
import { invoke } from "../lib/invokeLogged";
import {
  krakenIsTheMounter,
  krakenRefusal,
  krakenScanRoot,
  mountFileName,
  useMounterStore,
} from "../lib/mounter";
import { resolvePkgStorage, usePkgStorageStore } from "../lib/pkgStorage";
import { trStatic } from "../lib/trStatic";
import { pkgQueue } from "./consoleQueueBridge";

export interface MountPlaceResult {
  ok: boolean;
  message: string;
  /** Where the package went on the console. */
  dest?: string;
}

interface PkgHead {
  content_id?: string;
  title?: string;
  category?: string;
  platform?: string;
  authenticity?: string;
}

/** Why this package can't be mounted, in words; null when it can (as far as its header says). */
export function mountRefusalMessage(head: PkgHead | undefined, parts: number): string | null {
  const why = krakenRefusal({
    platform: head?.platform,
    authenticity: head?.authenticity,
    parts,
  });
  if (why === "split")
    return trStatic("pkg_mount_refuse_split", "A split package can't be mounted: join it into one .pkg first.");
  if (why === "ps4")
    return trStatic(
      "pkg_mount_refuse_ps4",
      "Kraken Mount mounts PS5 packages only. Install this PS4 package instead.",
    );
  if (why === "retail")
    return trStatic(
      "pkg_mount_refuse_retail",
      "Kraken Mount mounts packages made with the debug key only, and this one is a retail package.",
    );
  // A game is mounted; an add-on or a patch is not a game of its own (INFERRED from Kraken Mount
  // listing titles by their own eboot.bin and refusing patch packages, pkg.rs:86-145).
  const cat = (head?.category ?? "").toLowerCase();
  if (cat === "ac" || cat === "gp")
    return trStatic(
      "pkg_mount_refuse_not_game",
      "Only a game's package is mounted. Install updates and add-ons instead.",
    );
  return null;
}

/** What to do when the scan folder already holds that name: overwrite it, keep both (the new one
 *  under "Name (2).pkg"), or null to stop. */
export type MountNameClash = "replace" | "keep" | null;

/** The kind of `path` on the console ("file", "dir", ...), or null when nothing is there. Rejects
 *  when the console could not answer: "don't know" must not read as "free". */
async function consoleKindOf(host: string, path: string): Promise<string | null> {
  try {
    return (await fetchConsoleStat(host, path)).kind;
  } catch (e) {
    if (isPathMissingError(e)) return null;
    throw e;
  }
}

const uploadFailed = () => trStatic("pkg_mount_upload_failed", "The upload failed.");

/** Upload `localPath` (a `.pkg` on this computer) into Kraken Mount's scan root on the chosen
 *  package drive of `host`, wait for the upload, and ask Kraken Mount to scan.
 *
 *  A file of the same name in the scan folder (another dump, or the copy mounted right now) is
 *  never overwritten unasked: `onExists` is asked (the name, and whether that is a folder), and
 *  without it the package goes up under a free "Name (2).pkg". */
export async function placePkgForMount(
  localPath: string,
  host: string,
  opts: {
    drive?: string | null;
    onDest?: (dest: string) => void;
    onExists?: (name: string, existingDir: boolean) => Promise<MountNameClash>;
  } = {},
): Promise<MountPlaceResult> {
  const fail = (message: string): MountPlaceResult => ({ ok: false, message });
  let status: SmpStatus;
  try {
    status = await smpStatus(mgmtAddr(host));
    useMounterStore.getState().put(host, status);
  } catch (e) {
    return fail(e instanceof Error ? e.message : String(e));
  }
  if (!krakenIsTheMounter(status)) {
    return fail(
      trStatic("pkg_mount_no_kraken", "Kraken Mount isn't running on this PS5, so nothing would mount the package."),
    );
  }
  let meta: { parts?: string[]; head?: PkgHead };
  try {
    meta = (await invoke("pkg_metadata_split", { path: localPath })) as typeof meta;
  } catch (e) {
    return fail(e instanceof Error ? e.message : String(e));
  }
  const refusal = mountRefusalMessage(meta.head, meta.parts?.length ?? 1);
  if (refusal) return fail(refusal);

  let drive = opts.drive ?? null;
  if (!drive) {
    let volumes = null;
    try {
      volumes = await listVolumes(transferAddr(host));
    } catch {
      /* internal storage then */
    }
    drive = resolvePkgStorage(usePkgStorageStore.getState().defaultFor(host), volumes).volume ?? "/data";
  }
  const root = krakenScanRoot(drive, status.kraken) ?? krakenScanRoot("/data", status.kraken);
  if (!root) {
    return fail(
      trStatic(
        "pkg_mount_no_root",
        "Kraken Mount's scan folders (scanpath in /data/krakenmount/config.toml) include none on this drive or on internal storage.",
      ),
    );
  }
  let name = mountFileName(localPath, meta.head?.content_id ?? "");
  // The drive's homebrew folder may not be there yet; a scanpath of the user's is.
  await fsMkdir(transferAddr(host), root).catch(() => {});
  try {
    const there = await consoleKindOf(host, `${root}/${name}`);
    if (there !== null) {
      const isDir = there === "dir";
      const choice = opts.onExists ? await opts.onExists(name, isDir) : "keep";
      if (choice === null) return fail(trStatic("pkg.cancelled", "Cancelled."));
      // A folder can't be replaced by a file: that keeps both too.
      if (choice === "keep" || isDir) {
        let n = 2;
        while ((await consoleKindOf(host, `${root}/${keepBothName(name, n, false)}`)) !== null) n++;
        name = keepBothName(name, n, false);
      }
    }
  } catch (e) {
    return fail(e instanceof Error ? e.message : String(e));
  }
  const dest = `${root}/${name}`;
  opts.onDest?.(dest);

  const id = pkgQueue().add({
    sourceKind: "file",
    sourcePath: localPath,
    displayName: meta.head?.title || name,
    resolvedDest: dest,
    addr: transferAddr(host),
    strategy: "overwrite",
    reconcileMode: "fast",
    excludes: [],
    mountAfterUpload: false,
    mountReadOnly: true,
    registerAfterUpload: false,
  });
  if (!id) {
    return fail(trStatic("pkg_mount_not_queued", "Another window runs this console's queue: add it there."));
  }
  const done = await new Promise<{ ok: boolean; message?: string }>((resolve) => {
    let finished = false;
    let unsub: (() => void) | null = null;
    let wasRunning = false;
    const finish = (r: { ok: boolean; message?: string }) => {
      if (finished) return;
      finished = true;
      unsub?.();
      resolve(r);
    };
    unsub = pkgQueue().watch(id, (q) => {
      if (!q) finish({ ok: false, message: trStatic("pkg.removed_from_queue", "Removed from the queue.") });
      else if (q.status === "done") finish({ ok: true });
      else if (q.status === "failed") finish({ ok: false, message: q.error ?? uploadFailed() });
      else if (q.status === "running") wasRunning = true;
      // Pause or Stop in the queue puts a running item back to waiting: this pick is over
      // (a console gone to rest goes on by itself when it wakes, so that one is waited for).
      else if (q.status === "pending" && wasRunning && !q.waitingForWake)
        finish({
          ok: false,
          message: trStatic(
            "pkg_mount_paused",
            "Paused or stopped in the queue. Once it finishes there, Kraken Mount finds it on its next scan.",
          ),
        });
    });
    if (finished) unsub();
  });
  if (!done.ok) return { ok: false, message: done.message ?? uploadFailed(), dest };
  // Best effort: it scans every 10 s by itself anyway.
  await smpRescan(mgmtAddr(host)).catch(() => {});
  return {
    ok: true,
    dest,
    message: trStatic(
      "pkg_mount_done",
      "Put in {root}. Kraken Mount adds it to the home screen on its next scan; nothing was installed.",
    ).replace("{root}", root),
  };
}
