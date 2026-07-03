import { useCallback, useEffect, useMemo, useState } from "react";
// useEffect is consumed inside SaveThumbnail below.
import { useNavigate } from "react-router-dom";
import {
  Save,
  RefreshCw,
  Loader2,
  Download,
  Upload as UploadIcon,
  FolderOpen,
  HardDrive,
} from "lucide-react";
import { openInFileSystem } from "../../state/fsNavigation";
import {
  savesList,
  startTransferDir,
  startTransferFile,
  checkDestinationFreeSpace,
  fsDelete,
  fsListDir,
  waitForJob,
  saveArchiveMakeTemp,
  saveArchiveCleanupTemp,
  saveArchiveZip,
  saveArchiveUnzip,
  saveArchiveBackupFinalize,
  saveArchiveRestorePrepare,
  type SaveEntry,
} from "../../api/ps5";
import { localFs } from "../../api/localFs";
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { getSavePath } from "../../state/saveSettings";
import { backupTimestamp } from "../../lib/backupTimestamp";
import {
  PageHeader,
  Button,
  EmptyState,
  ErrorCard,
  ConnectionGate,
  GameIcon,
} from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { startTransferDownload } from "../../api/ps5";
import { formatBytes } from "../../lib/format";
import { useTitleInfo } from "../../lib/useTitleInfo";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { pickPath } from "../../lib/pickPath";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";

/**
 * Save data manager.
 *
 * Lists every save folder under /user/home/<uid>/savedata_prospero
 * (PS5 native) and /user/home/<uid>/savedata (PS4 legacy) on the
 * connected PS5. Per-game grouping; each row offers a download
 * (using the existing download transfer pipeline).
 *
 * Why not also list the raw param.sfo metadata: save folders don't
 * always have one, and the title_id (folder name) is enough for the
 * user to identify their save. The Library tab handles game-name
 * resolution separately.
 */
export default function SavesScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const guard = useStaleHostGuard();
  const [saves, setSaves] = useState<SaveEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Set of entry paths currently being backed up / restored. Used to
  // disable the per-row buttons so a rapid double-click doesn't race
  // two ops over the same PS5 save path (which would deletes-and-
  // uploads in undefined order and leave the live save corrupt).
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  // "Back up all to USB" is a single sequential run across every save, so
  // its busy flag is global rather than per-path like `busy` above.
  const [bulkBackupBusy, setBulkBackupBusy] = useState(false);
  // Same pattern for "Restore all from USB".
  const [bulkRestoreBusy, setBulkRestoreBusy] = useState(false);
  const isBusy = useCallback((path: string) => busy.has(path), [busy]);
  const markBusy = useCallback((path: string, on: boolean) => {
    setBusy((prev) => {
      const next = new Set(prev);
      if (on) next.add(path);
      else next.delete(path);
      return next;
    });
  }, []);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const refresh = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    // Host-stale guard (2.9.0, 2.12.0 migrated to canonical
    // useStaleHostGuard). savesList against host A can take seconds
    // on a console with many user accounts; if the user switches
    // roster to host B before it returns, the OLD list would
    // overwrite state and the UI would attribute A's saves to B —
    // dangerous when combined with handleRestore (a Restore click
    // on an entry shown under B would wipe B's actual save dir of
    // the same title_id, then upload A's data there).
    const probe = guard.capture();
    setLoading(true);
    setError(null);
    try {
      const r = await savesList(mgmtAddr(probe.host));
      if (probe.isStale()) return;
      setSaves(r.saves);
    } catch (e) {
      if (probe.isStale()) return;
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [host, payloadStatus, guard]);

  // Reset the rendered list the instant the active console changes, BEFORE
  // the new console's fetch resolves. The stale-host guard above already stops
  // a late result for the old console from overwriting state, but without this
  // the *previously rendered* rows (belonging to console A) stay on screen —
  // and clickable — until B's list arrives. A Restore/Delete click in that
  // window captures B's host (handleRestore reads the current host) but the
  // row's title_id/path from A, so it would operate on B with A's entry. Going
  // to a loading state removes that wrong-target hazard. Mirrors DiskUsage /
  // FileSystem, which already reset on [host].
  useEffect(() => {
    setSaves(null);
    setError(null);
  }, [host]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const grouped = useMemo(() => {
    if (!saves) return [] as Array<{ title_id: string; entries: SaveEntry[] }>;
    const map = new Map<string, SaveEntry[]>();
    for (const s of saves) {
      const key = s.title_id;
      const arr = map.get(key) ?? [];
      arr.push(s);
      map.set(key, arr);
    }
    return Array.from(map.entries())
      .map(([title_id, entries]) => ({ title_id, entries }))
      .sort((a, b) => a.title_id.localeCompare(b.title_id));
  }, [saves]);

  async function handleDownload(entry: SaveEntry) {
    if (!host?.trim()) return;
    if (isBusy(entry.path)) return;
    // Claim the entry IMMEDIATELY (before any dialog) so a rapid
    // second click while the picker is open is rejected. The dialogs
    // are async-await-able from the user's POV — the window from
    // "click Backup" to "user picks file" can be many seconds and a
    // second click in that window would otherwise race two ops over
    // the same PS5 path.
    markBusy(entry.path, true);
    let tempDir: string | null = null;
    try {
      // File-save dialog so the user picks a .zip target directly. The
      // default name `<title_id>.zip` matches the layout we enforce on
      // restore — keep the two in sync.
      const destZipName = `${entry.title_id}.zip`;
      const destZip = await saveDialog({
        defaultPath: destZipName,
        filters: [{ name: "ZIP archive", extensions: ["zip"] }],
        title: tr("saves_download_picker", undefined, "Save backup as…"),
      });
      if (!destZip || typeof destZip !== "string") return;
      // 1) Scratch dir under the OS temp root. The engine's download
      // walker will create `<scratch>/<title_id>/<files>` for us.
      tempDir = await saveArchiveMakeTemp(entry.title_id);
      // 2) Pull the PS5 save folder into the scratch dir.
      const jobId = await startTransferDownload(
        entry.path,
        tempDir,
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "folder",
      );
      await waitForJob(jobId);
      // 3) Format-aware cleanup: strip `sdimg_` prefix from PS4-format
      // images, drop Sony's nested emulator-bookkeeping subdirectories,
      // keep only the immediate files + `sce_sys/`. Matches garlic-
      // savemgr's view of the save and the cross-tool resigner format.
      await saveArchiveBackupFinalize(tempDir, entry.title_id);
      // 4) Zip the cleaned `<scratch>/<title_id>/` → user-picked .zip.
      await saveArchiveZip(tempDir, entry.title_id, destZip, destZipName);
      pushNotification(
        "success",
        withConsolePrefix(host, `Backed up ${entry.title_id}`),
        {
          body: `Saved to ${destZip}`,
        },
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification(
        "error",
        withConsolePrefix(host, `Backup failed: ${entry.title_id}`),
        {
          body: msg,
        },
      );
    } finally {
      // 4) Best-effort cleanup. The Rust side refuses any path outside
      // the OS temp root, so a stale `tempDir` reference can't trash
      // user data even if state somehow got mixed up.
      if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
      markBusy(entry.path, false);
    }
  }

  async function handleRestore(entry: SaveEntry) {
    if (!host?.trim()) return;
    if (isBusy(entry.path)) return;
    // Snapshot the host AT click time. The restore flow is many
    // seconds (confirm → file dialog → unzip → wipe → upload), and
    // the user can switch PS5 in the roster sidebar at any point.
    // Without this snapshot + re-check, the wipe-and-upload runs
    // against whatever IP is current at `await` resolution — i.e.
    // the WRONG console, recursively deleting somebody else's save
    // dir and then overwriting it with the wrong title's bytes.
    const restoreHost = host.trim();
    // Claim before any dialog — see comment in handleDownload.
    markBusy(entry.path, true);
    let tempDir: string | null = null;
    try {
      const ok = await confirmDialog({
        title: tr(
          "saves_restore_confirm_title",
          { title: entry.title_id },
          `Restore ${entry.title_id} from a .zip backup?`,
        ),
        message: tr(
          "saves_restore_confirm_body",
          undefined,
          "Pick a .zip whose top-level folder is named the same as the title ID. This WIPES the current PS5 save first, then uploads the backup — if the upload is interrupted (network drop, disk full), the save can be left empty with no automatic rollback. Back up the existing save first if it matters.",
        ),
        destructive: true,
        confirmLabel: tr("saves_restore_confirm_label", undefined, "Restore"),
      });
      if (!ok) return;

      const localZip = await pickPath({
        mode: "file",
        filters: [{ name: "ZIP archive", extensions: ["zip"] }],
        title: tr(
          "saves_restore_picker",
          undefined,
          "Pick the .zip backup to restore from",
        ),
      });
      if (!localZip || typeof localZip !== "string") return;
      // 1) Scratch dir + strict-validate the zip layout before we touch
      // the live save. Bad layout → throw before any delete fires.
      tempDir = await saveArchiveMakeTemp(entry.title_id);
      await saveArchiveUnzip(localZip, tempDir, entry.title_id);
      // 1.5) Format-aware prep: re-add `sdimg_` prefix to any bare image
      // file (PS4-style backups have it stripped) so the PS5 path matches
      // what Sony's emulator expects. PS5-native / .bin / sce_sys/ pass
      // through unchanged.
      await saveArchiveRestorePrepare(tempDir, entry.title_id);
      // 2) Wipe the existing save's CONTENTS but leave the title_id
      // folder itself in place. The savedata_prospero parent on PS5 is
      // managed by Sony's PFS subsystem and rejects raw POSIX mkdir of
      // a fresh title_id subdir (EACCES from ensure_parent_dir, see
      // payload/src/runtime.c:2877). By preserving the folder we side-
      // step that mkdir entirely — the upload's ensure_parent_dir hits
      // EEXIST and proceeds.
      //
      // Host-stale guard (2.9.0): refuse to wipe if the user changed
      // PS5 roster during the prior async steps (confirm/dialog/unzip
      // can together run for ~30s on big saves). Without this check
      // the recursive fsDelete fires against whatever IP is *current*
      // at this point — possibly a different console than the one the
      // user was looking at when they clicked Restore. Compare against
      // the freshly-read store value, not our captured `host` ref,
      // because `host` is a render-time closure and won't see writes.
      const currentHost = useConnectionStore.getState().host?.trim();
      if (currentHost !== restoreHost) {
        throw new Error(
          `Host changed during restore (was ${restoreHost}, now ${currentHost || "(none)"}). ` +
            "Aborted before wipe — your other console's saves are untouched.",
        );
      }
      const addr = `${restoreHost}:${PS5_PAYLOAD_PORT}`;
      const children = await fsListDir(addr, entry.path, { limit: 4096 });
      for (const child of children) {
        const childPath = entry.path.endsWith("/")
          ? `${entry.path}${child.name}`
          : `${entry.path}/${child.name}`;
        // fsDelete is recursive on the payload side, so passing a
        // subdir wipes its whole tree in one round trip.
        await fsDelete(addr, childPath);
      }
      const extractedRoot = `${tempDir}/${entry.title_id}`;
      const jobId = await startTransferDir(
        extractedRoot,
        entry.path,
        addr,
        null,
        [],
      );
      await waitForJob(jobId);
      pushNotification(
        "success",
        withConsolePrefix(restoreHost, `Restored ${entry.title_id}`),
        {
          body: `Uploaded ${entry.title_id}.zip back to ${entry.path}.`,
        },
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification(
        "error",
        withConsolePrefix(restoreHost, `Restore failed: ${entry.title_id}`),
        {
          body: msg,
        },
      );
    } finally {
      if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
      markBusy(entry.path, false);
    }
  }

  /**
   * Core "Save to USB storage" flow: pull the save off the PS5 (same
   * download/finalize/zip steps as handleDownload), but instead of a
   * host file-save dialog, push the resulting zip BACK onto the PS5 at
   * `<savePath>/<title_id>/<timestamp>/<title_id>.zip` — typically a USB
   * stick or extended-storage drive plugged into the console itself.
   *
   * `skipPreflight` lets the bulk "Back up all to USB" button validate
   * the USB mount once up front instead of once per title.
   */
  async function backupOneToUsb(
    entry: SaveEntry,
    opts?: { skipPreflight?: boolean },
  ) {
    if (!host?.trim()) return;
    if (isBusy(entry.path)) return;
    const backupHost = host.trim();
    const addr = `${backupHost}:${PS5_PAYLOAD_PORT}`;
    const base = getSavePath();
    markBusy(entry.path, true);
    let tempDir: string | null = null;
    try {
      if (!opts?.skipPreflight) {
        const preflight = await checkDestinationFreeSpace(addr, base, 0);
        if (!preflight.volume || !preflight.volume.writable || preflight.volume.is_placeholder) {
          throw new Error(
            tr(
              "saves_backup_usb_no_volume",
              { path: base },
              `No writable USB/external drive found at ${base}. Plug it into the PS5 and try again.`,
            ),
          );
        }
      }
      // 1) Scratch dir + pull the PS5 save folder, same as handleDownload.
      tempDir = await saveArchiveMakeTemp(entry.title_id);
      const jobId = await startTransferDownload(entry.path, tempDir, addr, "folder");
      await waitForJob(jobId);
      // 2) Format-aware cleanup (strip sdimg_ prefix, drop emulator
      // bookkeeping subdirs) — identical to handleDownload.
      await saveArchiveBackupFinalize(tempDir, entry.title_id);
      // 3) Zip into the scratch dir itself (no host file-save dialog —
      // the zip never needs to leave the temp dir before it's uploaded).
      const zipName = `${entry.title_id}.zip`;
      const hostZip = `${tempDir}/${zipName}`;
      await saveArchiveZip(tempDir, entry.title_id, hostZip, zipName);
      // 4) Size the zip (via the scratch dir listing) and confirm the USB
      // target has room for it. Best-effort: an unreadable size or an
      // unmatched volume just skips the check rather than blocking.
      const remoteDir = `${base}/${entry.title_id}/${backupTimestamp()}`;
      const remoteZip = `${remoteDir}/${zipName}`;
      const tempEntries = await localFs.listDir(tempDir).catch(() => []);
      const zipSize = tempEntries.find((e) => e.name === zipName)?.size ?? 0;
      if (zipSize > 0) {
        const spaceCheck = await checkDestinationFreeSpace(addr, remoteZip, zipSize);
        if (spaceCheck.insufficient) {
          throw new Error(
            tr(
              "saves_backup_usb_low_space",
              { path: base },
              `Not enough free space at ${base} for this backup.`,
            ),
          );
        }
      }
      // 5) Stale-host re-check, same reasoning as handleRestore: the
      // download+zip steps above can run for many seconds, and the user
      // may have switched PS5 in the roster sidebar meanwhile. Refuse to
      // upload to the wrong console.
      const currentHost = useConnectionStore.getState().host?.trim();
      if (currentHost !== backupHost) {
        throw new Error(
          `Host changed during backup (was ${backupHost}, now ${currentHost || "(none)"}). ` +
            "Aborted before upload — your other console's USB drive is untouched.",
        );
      }
      // 6) Upload the zip to the PS5's USB path. The payload's
      // ensure_parent_dir auto-creates <title_id>/<timestamp>/ — no
      // manual mkdir needed.
      const jobId2 = await startTransferFile(hostZip, remoteZip, addr, null);
      await waitForJob(jobId2);
      pushNotification(
        "success",
        withConsolePrefix(backupHost, `Backed up ${entry.title_id} to USB`),
        { body: `Saved to ${remoteZip}` },
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification(
        "error",
        withConsolePrefix(backupHost, `USB backup failed: ${entry.title_id}`),
        { body: msg },
      );
      throw e; // let the bulk handler count this as a failure
    } finally {
      if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
      markBusy(entry.path, false);
    }
  }

  function handleBackupToUsb(entry: SaveEntry) {
    backupOneToUsb(entry).catch(() => {
      // Already surfaced via setError + pushNotification above.
    });
  }

  /**
   * Finds the lexically-latest timestamped backup directory for a given
   * title under `<base>/<title_id>/`. The backupTimestamp() format
   * (YYYY-MM-DD_HHMMSS) is documented as lexically sortable, so the
   * greatest string name == the newest backup — no mtime parsing needed.
   *
   * Returns null if the title dir is absent, unreadable, or has never
   * been backed up (no subdirs).
   */
  async function resolveLatestUsbBackup(
    addr: string,
    base: string,
    title_id: string,
  ): Promise<string | null> {
    try {
      const entries = await fsListDir(addr, `${base}/${title_id}`);
      const dirs = entries
        .filter((e) => e.kind === "dir")
        .map((e) => e.name)
        .sort()
        .reverse();
      return dirs[0] ?? null;
    } catch {
      // Dir absent or unreadable — no backup has ever been written.
      return null;
    }
  }

  /**
   * Core "Restore from USB" flow: the reverse of backupOneToUsb.
   *
   * Finds the latest timestamped zip for the title on the USB save path,
   * pulls it to a host temp dir, validates + unzips it, then wipes and
   * re-uploads the live PS5 save — exactly as handleRestore does, but
   * sourcing the zip from the PS5's USB drive rather than a host file dialog.
   *
   * Returns "no-backup" if no USB backup exists for the title (so the bulk
   * handler can skip rather than fail). Throws on genuine errors.
   *
   * `skipPreflight` / `skipConfirm` let handleRestoreAllFromUsb validate
   * the USB volume and confirm once across all games instead of per-title.
   */
  async function restoreOneFromUsb(
    entry: SaveEntry,
    opts?: { skipPreflight?: boolean; skipConfirm?: boolean },
  ): Promise<"ok" | "no-backup"> {
    if (!host?.trim()) return "no-backup";
    if (isBusy(entry.path)) return "no-backup";
    const restoreHost = host.trim();
    const addr = `${restoreHost}:${PS5_PAYLOAD_PORT}`;
    const base = getSavePath();
    // Claim before any async work — same reasoning as handleRestore.
    markBusy(entry.path, true);
    let tempDir: string | null = null;
    try {
      if (!opts?.skipPreflight) {
        // Only require the volume to exist and not be a placeholder;
        // restore reads from USB so we don't require `writable`.
        const preflight = await checkDestinationFreeSpace(addr, base, 0);
        if (!preflight.volume || preflight.volume.is_placeholder) {
          throw new Error(
            tr(
              "saves_backup_usb_no_volume",
              { path: base },
              `No USB/external drive found at ${base}. Plug it into the PS5 and try again.`,
            ),
          );
        }
      }
      // Resolve the newest backup. Return "no-backup" rather than
      // throwing so the bulk handler can skip quietly without incrementing
      // the failure count.
      const latest = await resolveLatestUsbBackup(addr, base, entry.title_id);
      if (!latest) return "no-backup";
      const zipName = `${entry.title_id}.zip`;
      const remoteZip = `${base}/${entry.title_id}/${latest}/${zipName}`;
      if (!opts?.skipConfirm) {
        const ok = await confirmDialog({
          title: tr(
            "saves_restore_usb_confirm_title",
            { title: entry.title_id, timestamp: latest },
            `Restore ${entry.title_id} from USB backup ${latest}?`,
          ),
          message: tr(
            "saves_restore_usb_confirm_body",
            { title: entry.title_id, timestamp: latest },
            `This WIPES the current PS5 save for ${entry.title_id} and replaces it with the backup from ${latest}. ` +
              "There is no automatic rollback if the upload is interrupted. Back up the existing save first if it matters.",
          ),
          destructive: true,
          confirmLabel: tr(
            "saves_restore_usb_confirm_label",
            undefined,
            "Restore from USB",
          ),
        });
        if (!ok) return "ok"; // user cancelled — not an error or a skip
      }
      // 1) Scratch dir + pull the zip off the USB drive (single-file download).
      tempDir = await saveArchiveMakeTemp(entry.title_id);
      const jobId = await startTransferDownload(remoteZip, tempDir, addr, "file");
      await waitForJob(jobId);
      // 2) Strict layout validation: abort before touching the live save
      // if the zip doesn't contain exactly one top-level folder named
      // after the title_id (same guard as handleRestore).
      const hostZip = `${tempDir}/${zipName}`;
      await saveArchiveUnzip(hostZip, tempDir, entry.title_id);
      // 3) Format-aware prep: re-add sdimg_ prefix stripped during backup
      // finalization (PS4-style saves). PS5-native / .bin / sce_sys/ pass through.
      await saveArchiveRestorePrepare(tempDir, entry.title_id);
      // 4) Stale-host guard before the destructive wipe. The download +
      // unzip steps above can run for many seconds on large saves, and
      // the user may have switched PS5 in the roster sidebar. Same guard
      // as handleRestore — compare against the live store value, not the
      // closure-captured `host` ref.
      const currentHost = useConnectionStore.getState().host?.trim();
      if (currentHost !== restoreHost) {
        throw new Error(
          `Host changed during USB restore (was ${restoreHost}, now ${currentHost || "(none)"}). ` +
            "Aborted before wipe — your other console's saves are untouched.",
        );
      }
      // 5) Wipe live save contents but keep the title_id folder itself —
      // see handleRestore lines 269-275 for why we can't mkdir it fresh.
      const children = await fsListDir(addr, entry.path, { limit: 4096 });
      for (const child of children) {
        const childPath = entry.path.endsWith("/")
          ? `${entry.path}${child.name}`
          : `${entry.path}/${child.name}`;
        await fsDelete(addr, childPath);
      }
      // 6) Upload the unpacked save back to the live PS5 path.
      const extractedRoot = `${tempDir}/${entry.title_id}`;
      const jobId2 = await startTransferDir(extractedRoot, entry.path, addr, null, []);
      await waitForJob(jobId2);
      pushNotification(
        "success",
        withConsolePrefix(restoreHost, `Restored ${entry.title_id} from USB`),
        { body: `Restored from ${remoteZip} → ${entry.path}` },
      );
      return "ok";
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification(
        "error",
        withConsolePrefix(restoreHost, `USB restore failed: ${entry.title_id}`),
        { body: msg },
      );
      throw e; // let the bulk handler count this as a failure
    } finally {
      if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
      markBusy(entry.path, false);
    }
  }

  function handleRestoreFromUsb(entry: SaveEntry) {
    restoreOneFromUsb(entry).catch(() => {
      // Already surfaced via setError + pushNotification above.
    });
  }

  async function handleRestoreAllFromUsb() {
    if (!host?.trim() || !saves || saves.length === 0 || bulkRestoreBusy) return;
    const restoreHost = host.trim();
    const addr = `${restoreHost}:${PS5_PAYLOAD_PORT}`;
    const base = getSavePath();
    setBulkRestoreBusy(true);
    try {
      // Single up-front confirm covering all games — the user only has to
      // approve once rather than per-title like the per-row button.
      const confirmed = await confirmDialog({
        title: tr(
          "saves_restore_usb_all_confirm_title",
          undefined,
          "Restore all saves from USB?",
        ),
        message: tr(
          "saves_restore_usb_all_confirm_body",
          undefined,
          "This will WIPE the current PS5 save for each game and replace it with its latest USB backup. " +
            "Games with no backup on the USB drive are skipped. There is no automatic rollback if an upload is interrupted.",
        ),
        destructive: true,
        confirmLabel: tr(
          "saves_restore_usb_all_confirm_label",
          undefined,
          "Restore all",
        ),
      });
      if (!confirmed) return;
      // Volume check once up front (same as handleBackupAllToUsb).
      const preflight = await checkDestinationFreeSpace(addr, base, 0);
      if (!preflight.volume || preflight.volume.is_placeholder) {
        const msg = tr(
          "saves_backup_usb_no_volume",
          { path: base },
          `No USB/external drive found at ${base}. Plug it into the PS5 and try again.`,
        );
        setError(msg);
        pushNotification("error", withConsolePrefix(restoreHost, "USB restore failed"), {
          body: msg,
        });
        return;
      }
      let ok = 0;
      let failed = 0;
      let skipped = 0;
      for (const entry of saves) {
        try {
          const result = await restoreOneFromUsb(entry, {
            skipPreflight: true,
            skipConfirm: true,
          });
          if (result === "no-backup") skipped++;
          else ok++;
        } catch {
          failed++;
        }
      }
      const hasSideEffects = failed > 0 || skipped > 0;
      pushNotification(
        failed === 0 ? "success" : "error",
        withConsolePrefix(
          restoreHost,
          !hasSideEffects
            ? tr(
                "saves_restore_usb_summary",
                { ok, total: saves.length },
                `Restored ${ok}/${saves.length} from USB`,
              )
            : tr(
                "saves_restore_usb_summary_partial",
                { ok, total: saves.length, failed, skipped },
                [
                  `Restored ${ok}/${saves.length} from USB`,
                  failed > 0 ? `${failed} failed` : "",
                  skipped > 0 ? `${skipped} skipped` : "",
                ]
                  .filter(Boolean)
                  .join("; "),
              ),
        ),
      );
    } finally {
      setBulkRestoreBusy(false);
    }
  }

  async function handleBackupAllToUsb() {
    if (!host?.trim() || !saves || saves.length === 0 || bulkBackupBusy) return;
    const backupHost = host.trim();
    const addr = `${backupHost}:${PS5_PAYLOAD_PORT}`;
    const base = getSavePath();
    setBulkBackupBusy(true);
    try {
      const preflight = await checkDestinationFreeSpace(addr, base, 0);
      if (!preflight.volume || !preflight.volume.writable || preflight.volume.is_placeholder) {
        const msg = tr(
          "saves_backup_usb_no_volume",
          { path: base },
          `No writable USB/external drive found at ${base}. Plug it into the PS5 and try again.`,
        );
        setError(msg);
        pushNotification("error", withConsolePrefix(backupHost, "USB backup failed"), {
          body: msg,
        });
        return;
      }
      let ok = 0;
      let failed = 0;
      for (const entry of saves) {
        try {
          await backupOneToUsb(entry, { skipPreflight: true });
          ok++;
        } catch {
          failed++;
        }
      }
      pushNotification(
        failed === 0 ? "success" : "error",
        withConsolePrefix(
          backupHost,
          failed === 0
            ? tr("saves_backup_usb_summary", { ok, total: saves.length }, `Backed up ${ok}/${saves.length} to USB`)
            : tr(
                "saves_backup_usb_summary_failed",
                { ok, total: saves.length, failed },
                `Backed up ${ok}/${saves.length} to USB; ${failed} failed`,
              ),
        ),
      );
    } finally {
      setBulkBackupBusy(false);
    }
  }

  return (
    <div className="p-6">
      {confirmDialogNode}
      <PageHeader
        icon={Save}
        title={tr("saves_title", undefined, "Save data")}
        count={saves?.length}
        loading={loading}
        description={tr(
          "saves_description",
          undefined,
          "Per-game save folders on the PS5. PS5 saves under savedata_prospero/, PS4 legacy saves under savedata/. Backup writes a portable <title-id>.zip; restore expects the same shape.",
        )}
        right={
          <div className="flex items-center gap-2">
            <Button
              variant="secondary"
              size="sm"
              leftIcon={
                bulkBackupBusy ? (
                  <Loader2 size={12} className="animate-spin" />
                ) : (
                  <HardDrive size={12} />
                )
              }
              onClick={handleBackupAllToUsb}
              disabled={
                bulkBackupBusy ||
                bulkRestoreBusy ||
                loading ||
                !host?.trim() ||
                payloadStatus !== "up" ||
                !saves ||
                saves.length === 0
              }
            >
              {tr("saves_backup_usb_all", undefined, "Back up all to USB")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={
                bulkRestoreBusy ? (
                  <Loader2 size={12} className="animate-spin" />
                ) : (
                  <UploadIcon size={12} />
                )
              }
              onClick={handleRestoreAllFromUsb}
              disabled={
                bulkRestoreBusy ||
                bulkBackupBusy ||
                loading ||
                !host?.trim() ||
                payloadStatus !== "up" ||
                !saves ||
                saves.length === 0
              }
            >
              {tr("saves_restore_usb_all", undefined, "Restore all from USB")}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<RefreshCw size={12} />}
              onClick={refresh}
              disabled={loading || !host?.trim() || payloadStatus !== "up"}
            >
              {tr("refresh", undefined, "Refresh")}
            </Button>
          </div>
        }
      />

      <ConnectionGate require="payload">
        {error && (
          <div className="mb-4">
            <ErrorCard
              title={tr("saves_error", undefined, "Couldn't list saves")}
              detail={error}
            />
          </div>
        )}

        {saves && saves.length === 0 && (
          <EmptyState
            icon={Save}
            message={tr(
              "saves_empty",
              undefined,
              "No saves found. Either no users have ever saved a game, or the savedata folders aren't where we expect them.",
            )}
            action={
              <Button variant="secondary" onClick={() => navigate("/library")}>
                {tr("saves_empty_cta", "Browse Library")}
              </Button>
            }
          />
        )}

        <div className="mx-auto max-w-4xl space-y-3">
          {grouped.map(({ title_id, entries }) => (
            <section
              key={title_id}
              className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
            >
              <SaveGroupHeader
                host={host}
                titleId={title_id}
                folderCount={entries.length}
                firstPath={entries[0]?.path}
              />
              <ul className="space-y-1">
                {entries.map((e) => (
                  <li
                    key={e.path}
                    className="flex items-center gap-3 rounded-md bg-[var(--color-surface)] px-2 py-1.5 text-xs"
                  >
                    <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs uppercase">
                      {e.kind}
                    </span>
                    <div className="min-w-0 flex-1">
                      <code className="block truncate text-xs text-[var(--color-muted)]">
                        {e.path}
                      </code>
                      <div className="text-xs text-[var(--color-muted)]">
                        {tr("saves_user", undefined, "user")} {e.user_id} ·{" "}
                        {formatBytes(e.size)} ·{" "}
                        {new Date(e.mtime * 1000).toLocaleDateString()}
                      </div>
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      leftIcon={<Download size={11} />}
                      onClick={() => handleDownload(e)}
                      disabled={isBusy(e.path)}
                    >
                      {tr("saves_download", undefined, "Backup")}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      leftIcon={<HardDrive size={11} />}
                      onClick={() => handleBackupToUsb(e)}
                      disabled={isBusy(e.path) || bulkBackupBusy}
                      title={tr(
                        "saves_backup_usb_tooltip",
                        undefined,
                        "Back this save up to the USB save path configured in Settings, without leaving the PS5.",
                      )}
                    >
                      {tr("saves_backup_usb", undefined, "Save to USB")}
                    </Button>
                    {/* danger (red-bordered), NOT ghost like Backup: Restore
                        overwrites — wipes — the live PS5 save. It sat visually
                        identical to the harmless Backup button next to it,
                        giving no at-a-glance signal which of the two is
                        destructive. The confirm dialog still guards the click. */}
                    <Button
                      variant="danger"
                      size="sm"
                      leftIcon={<UploadIcon size={11} />}
                      onClick={() => handleRestore(e)}
                      disabled={isBusy(e.path)}
                      title={tr(
                        "saves_restore_tooltip",
                        undefined,
                        "Pick a .zip backup and upload its contents back to this save's PS5 path. Overwrites the live save.",
                      )}
                    >
                      {tr("saves_restore", undefined, "Restore")}
                    </Button>
                    <Button
                      variant="danger"
                      size="sm"
                      leftIcon={<HardDrive size={11} />}
                      onClick={() => handleRestoreFromUsb(e)}
                      disabled={isBusy(e.path) || bulkRestoreBusy}
                      title={tr(
                        "saves_restore_usb_tooltip",
                        undefined,
                        "Restore this save from its latest USB backup at the path configured in Settings, without leaving the PS5. Overwrites the live save.",
                      )}
                    >
                      {tr("saves_restore_usb", undefined, "Restore from USB")}
                    </Button>
                  </li>
                ))}
              </ul>
            </section>
          ))}
        </div>

        {loading && saves === null && (
          <div className="mt-4 text-center text-xs text-[var(--color-muted)]">
            <Loader2 size={12} className="mr-2 inline animate-spin" />
            {tr("saves_loading", undefined, "Reading saves…")}
          </div>
        )}
      </ConnectionGate>
    </div>
  );
}

/**
 * One save group's header: cover + game name + folder count + "Open folder".
 *
 * Extracted into its own component so it can resolve the title id to a display
 * name and a cover via {@link useTitleInfo} — a hook can't run inside the
 * parent's `.map()`. The name shows as "Saros (PPSA07631)" so the id stays
 * visible (it's what the save folder is actually named) while the human title
 * leads. The resolved cover URL is also handed to GameIcon as a last-resort
 * source, so saves for games that aren't installed (no local appmeta art)
 * still get a thumbnail from the cover CDN instead of a bare glyph.
 */
function SaveGroupHeader({
  host,
  titleId,
  folderCount,
  firstPath,
}: {
  host: string;
  titleId: string;
  folderCount: number;
  firstPath?: string;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const info = useTitleInfo(titleId);
  return (
    <header className="mb-2 flex items-center gap-2">
      {/* Game cover from /user/appmeta/<id>/icon0.png (readable), not the
          save's own icon0.png — that lives inside an unmounted PFS container,
          so reading it failed for every save and only spammed warnings. Falls
          back to the external cover (info.coverImageUrl) for not-installed
          games, then a glyph. */}
      <GameIcon
        host={host}
        titleId={titleId}
        fallbackSrc={info?.coverImageUrl}
        size={32}
        rounded="rounded"
      />
      {/* Human title leads, region id kept in parentheses so the user can
          still tie it to the on-disk save folder name. */}
      <h3 className="min-w-0 truncate text-sm font-semibold" title={titleId}>
        {info?.title ? `${info.title} (${titleId})` : titleId}
      </h3>
      <span className="shrink-0 text-xs text-[var(--color-muted)]">
        {folderCount} {tr("saves_folder", undefined, "folder")}
        {folderCount === 1 ? "" : "s"}
      </span>
      {/* Jump to this title's save directory in the File System browser —
          quick way to inspect/manage the raw files. */}
      {firstPath && (
        <Button
          variant="ghost"
          size="sm"
          className="ms-auto shrink-0"
          leftIcon={<FolderOpen size={12} />}
          onClick={() => openInFileSystem(navigate, firstPath)}
          title={tr(
            "saves_open_folder_hint",
            undefined,
            "Open this save's folder in the File System browser",
          )}
        >
          {tr("saves_open_folder", undefined, "Open folder")}
        </Button>
      )}
    </header>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary).
// SaveThumbnail was removed: it read each save's own
// /user/home/<uid>/savedata_prospero/<id>/sce_sys/icon0.png, which lives
// inside an unmounted PFS container — so the read failed for every save and
// only produced warnings. The save rows now use the shared <GameIcon> (the
// game's appmeta cover), which is readable and actually shows art.
