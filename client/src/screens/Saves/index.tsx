import { consoleAddr } from "../../lib/addr";
import { isAndroid } from "../../lib/platform";
import { backupStamp } from "./backupStamp";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
// useEffect is consumed inside SaveThumbnail below.
import { Link, useLocation, useNavigate } from "react-router";
import { gamePath, hasTitleId } from "../../lib/gamePage";
import {
  Save,
  RefreshCw,
  Download,
  Upload as UploadIcon,
  FolderOpen,
  HardDrive,
  LockOpen,
  Lock,
  Share2,
  FileDown,
} from "lucide-react";
import { putSharedSave, shareSave } from "../../api/saveShare";
import { saveFile } from "../../lib/rpCapture";
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
  savesOpen,
  savesClose,
  type OpenSave,
  profileInfo,
  type ProfileUser,
} from "../../api/ps5";
import {
  backupZipName,
  defaultUserFilter,
  filterSaves,
  latestUsbBackup,
  saveOwners,
  usbUserDir,
  userHex,
  userName,
  type UserFilter,
} from "./saveUsers";
import { beginTask, type TaskHandle } from "../../state/trackTask";
import { localFs } from "../../api/localFs";
import { useConnectionStore } from "../../state/connection";
import { getSavePath } from "../../state/saveSettings";
import { backupTimestamp } from "../../lib/backupTimestamp";
import {
  PageHeader,
  Button,
  EmptyState,
  ErrorCard,
  ConnectionGate,
  GameIcon,
  OverflowMenu,
  Spinner,
  Modal,
  Select,
} from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { startTransferDownload } from "../../api/ps5";
import { formatBytes } from "../../lib/format";
import { iconTitleId } from "../../lib/titleId";
import { useTitleInfo } from "../../lib/useTitleInfo";
import { fetchTitleInfo } from "../../lib/titleDetails";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { pickSaveDest as saveDialog } from "../../lib/pickSaveDest";
import { pickPath } from "../../lib/pickPath";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { isTauriEnv } from "../../lib/tauriEnv";
import AutoBackupCard from "./AutoBackupCard";
import ServerBackupCard from "./ServerBackupCard";
import MoveFromPs4 from "./MoveFromPs4";
import SwitchSaves from "./SwitchSaves";
import { formatDate } from "../../lib/formatDate";

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
  // A save opened decrypted on the console (one at a time), and the picker for a folder
  // holding more than one save image.
  const [openSave, setOpenSave] = useState<OpenSave | null>(null);
  const [openBusy, setOpenBusy] = useState(false);
  const [picker, setPicker] = useState<{
    entry: SaveEntry;
    images: string[];
    /** What to do with the image picked: open it, share it, or put a shared save into it. */
    then: "open" | "share" | "use";
  } | null>(null);
  // Using a shared save: the save image it goes into, while the file is being picked.
  const shareInput = useRef<HTMLInputElement>(null);
  const [useTarget, setUseTarget] = useState<{ entry: SaveEntry; image: string } | null>(null);
  useEffect(() => {
    if (!host?.trim() || payloadStatus !== "up") {
      setOpenSave(null);
      return;
    }
    let live = true;
    savesOpen(consoleAddr(host.trim()), "")
      .then((r) => live && setOpenSave(r.mounted ? r : null))
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [host, payloadStatus]);
  const doOpenSave = async (image: string) => {
    if (!host?.trim()) return;
    setOpenBusy(true);
    try {
      const r = await savesOpen(consoleAddr(host.trim()), image);
      setOpenSave(r.mounted ? r : null);
      pushNotification("success", tr("saves_opened", undefined, "Save open"), {
        body: tr("saves_opened_body", { path: r.mount_point }, "Browse and edit it under {path} in Files. Close it from Save data when done."),
      });
      openInFileSystem(navigate, r.mount_point);
    } catch (e) {
      pushNotification("error", tr("saves_open_failed", { error: e instanceof Error ? e.message : String(e) }, "Could not open the save: {error}"));
    } finally {
      setOpenBusy(false);
    }
  };
  // Sharing a save with another account (PS5 saves): its files out as a zip, or a shared
  // save's files into your own save of the same game.
  const doShare = async (image: string) => {
    if (!host?.trim()) return;
    setOpenBusy(true);
    try {
      const { blob, name } = await shareSave(consoleAddr(host.trim()), image);
      const where = await saveFile(blob, name);
      pushNotification("success", tr("saves_shared", undefined, "Save shared"), {
        body: tr(
          "saves_shared_body",
          { where: where ?? name },
          "Saved as {where}. Send it to your friend: on their PS5, Save Data › Use a shared save puts it into their own save of the game.",
        ),
      });
    } catch (e) {
      pushNotification("error", tr("saves_share_failed", { error: e instanceof Error ? e.message : String(e) }, "Could not share the save: {error}"));
    } finally {
      setOpenBusy(false);
    }
  };
  const doUseShared = async (file: File) => {
    const target = useTarget;
    setUseTarget(null);
    if (!host?.trim() || !target) return;
    const ok = await confirmDialog({
      title: tr("saves_use_shared_confirm_title", { title: target.entry.title_id }, `Use this shared save for ${target.entry.title_id}?`),
      message: tr(
        "saves_use_shared_confirm_body",
        undefined,
        "Your save's files are replaced with the shared save's, and the PS5 keeps it as yours. Your own save is backed up first (Save Data backups) and put back if anything fails. Close the game first.",
      ),
      destructive: true,
      confirmLabel: tr("saves_use_shared_confirm_label", undefined, "Use it"),
    });
    if (!ok) return;
    setOpenBusy(true);
    try {
      await putSharedSave(consoleAddr(host.trim()), target.image, target.entry.user_id, file);
      pushNotification("success", tr("saves_use_shared_done", { title: target.entry.title_id }, "{title} now has the shared save"), {
        body: tr("saves_use_shared_done_body", undefined, "Start the game to load it. Your own save was backed up first."),
      });
      void refresh();
    } catch (e) {
      pushNotification("error", tr("saves_use_shared_failed", { error: e instanceof Error ? e.message : String(e) }, "Could not use the shared save: {error}"));
    } finally {
      setOpenBusy(false);
    }
  };
  const runOn = (entry: SaveEntry, image: string, then: "open" | "share" | "use") => {
    if (then === "open") return doOpenSave(image);
    if (then === "share") return doShare(image);
    setUseTarget({ entry, image });
    shareInput.current?.click();
  };
  const handleOpenSave = async (entry: SaveEntry, then: "open" | "share" | "use" = "open") => {
    if (!host?.trim() || openBusy) return;
    setOpenBusy(true);
    try {
      const children = await fsListDir(consoleAddr(host.trim()), entry.path, { limit: 4096 });
      const images = children
        .filter((c) => c.kind === "file" && !/\.bin$/i.test(c.name) && (entry.kind !== "ps4" || c.name.startsWith("sdimg_")))
        .map((c) => (entry.path.endsWith("/") ? `${entry.path}${c.name}` : `${entry.path}/${c.name}`));
      if (images.length === 0) {
        pushNotification("info", tr("saves_open_none", undefined, "No save image in this folder."));
        return;
      }
      setOpenBusy(false);
      if (images.length === 1) await runOn(entry, images[0], then);
      else setPicker({ entry, images, then });
    } catch (e) {
      pushNotification("error", tr("saves_open_failed", { error: e instanceof Error ? e.message : String(e) }, "Could not open the save: {error}"));
    } finally {
      setOpenBusy(false);
    }
  };
  const handleCloseSave = async () => {
    if (!host?.trim() || openBusy) return;
    setOpenBusy(true);
    try {
      const r = await savesClose(consoleAddr(host.trim()));
      setOpenSave(null);
      pushNotification("success", tr("saves_closed", undefined, "Save closed"), {
        body: r.written_back
          ? tr("saves_closed_written", undefined, "The image was written back into the save.")
          : tr("saves_closed_unchanged", undefined, "Nothing changed; the save is as it was."),
      });
    } catch (e) {
      pushNotification("error", tr("saves_open_failed", { error: e instanceof Error ? e.message : String(e) }, "Could not close the save: {error}"));
    } finally {
      setOpenBusy(false);
    }
  };
  const guard = useStaleHostGuard();
  const [allSaves, setSaves] = useState<SaveEntry[] | null>(null);
  // The console's local users (names) and whose saves are shown. On a shared console the
  // screen starts on the user at the console, so backups and restores can't pick up someone
  // else's save of the same game by mistake.
  const [users, setUsers] = useState<ProfileUser[]>([]);
  const [userFilter, setUserFilter] = useState<UserFilter>("all");
  const filterChosen = useRef(false);
  const saves = useMemo(
    () => (allSaves ? filterSaves(allSaves, userFilter) : null),
    [allSaves, userFilter],
  );
  const owners = useMemo(() => saveOwners(allSaves ?? [], users), [allSaves, users]);
  const ownerName = useCallback((e: SaveEntry) => userName(e.user_id, users), [users]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Bumped by each refresh, so the Switch emulator's saves are looked at again too.
  const [refreshCount, setRefreshCount] = useState(0);
  // Opened from a game's page: that game's saves are scrolled to and marked.
  const location = useLocation();
  const focusTitle =
    typeof (location.state as { titleId?: unknown } | null)?.titleId === "string"
      ? ((location.state as { titleId: string }).titleId).toUpperCase()
      : null;
  const focused = useRef(false);
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
    setRefreshCount((n) => n + 1);
    try {
      const [r, profile] = await Promise.all([
        savesList(mgmtAddr(probe.host)),
        // Names are a nicety: an older helper or a failed read just shows ids.
        profileInfo(mgmtAddr(probe.host)).catch(() => null),
      ]);
      if (probe.isStale()) return;
      setUsers(profile?.users ?? []);
      if (!filterChosen.current) {
        filterChosen.current = true;
        setUserFilter(defaultUserFilter(r.saves, profile?.uid ?? 0));
      }
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
    setUsers([]);
    setUserFilter("all");
    filterChosen.current = false;
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

  useEffect(() => {
    if (!focusTitle || focused.current) return;
    if (!grouped.some((g) => g.title_id.toUpperCase() === focusTitle)) return;
    focused.current = true;
    document
      .querySelector(`[data-title-id="${focusTitle}"]`)
      ?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [grouped, focusTitle]);

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
    let task: TaskHandle | null = null;
    try {
      // File-save dialog so the user picks a .zip target directly. The
      // default name `<title_id>.zip` matches the layout we enforce on
      // restore — keep the two in sync.
      // Android's save dialog hands back a content:// file that the zip cannot be written into:
      // it stayed empty while the zip went to Downloads (#451, "0 byte zip"). There the backup
      // goes straight to Downloads, dated so a second backup does not replace the first.
      const android = isAndroid();
      // The game's name goes in too when it is known (usually cached by the group header);
      // never hold the dialog up for it.
      const gameName = await Promise.race([
        fetchTitleInfo(iconTitleId(entry.title_id) ?? entry.title_id)
          .then((i) => i?.title ?? null)
          .catch(() => null),
        new Promise<null>((r) => setTimeout(() => r(null), 1500)),
      ]);
      const destZipName = backupZipName(
        entry.title_id,
        ownerName(entry),
        android ? backupStamp(new Date()) : undefined,
        gameName,
      );
      const destZip = android
        ? ""
        : await saveDialog({
            defaultPath: destZipName,
            filters: [{ name: "ZIP archive", extensions: ["zip"] }],
            title: tr("saves_download_picker", undefined, "Save backup as…"),
          });
      if (!android && (!destZip || typeof destZip !== "string")) return;
      // 1) Scratch dir under the OS temp root. The engine's download
      // walker will create `<scratch>/<title_id>/<files>` for us.
      task = beginTask({ kind: "save-backup", origin: "saves", label: `Save backup ${entry.title_id}`, consoleId: host.trim() });
      tempDir = await saveArchiveMakeTemp(entry.title_id);
      // 2) Pull the PS5 save folder into the scratch dir.
      const jobId = await startTransferDownload(
        entry.path,
        tempDir,
        consoleAddr(host.trim()),
        "folder",
      );
      await waitForJob(jobId);
      // 3) Format-aware cleanup: strip `sdimg_` prefix from PS4-format
      // images, drop Sony's nested emulator-bookkeeping subdirectories,
      // keep only the immediate files + `sce_sys/`. Matches garlic-
      // savemgr's view of the save and the cross-tool resigner format.
      await saveArchiveBackupFinalize(tempDir, entry.title_id);
      // 4) Zip the cleaned `<scratch>/<title_id>/` → user-picked .zip.
      const savedTo = await saveArchiveZip(tempDir, entry.title_id, destZip ?? "", destZipName);
      pushNotification(
        "success",
        withConsolePrefix(host, `Backed up ${entry.title_id}`),
        {
          body: `Saved to ${savedTo}`,
        },
      );
    } catch (e) {
      task?.fail(e);
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
      task?.done();
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
    let task: TaskHandle | null = null;
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
          "Pick a .zip whose top-level folder is named the same as the title ID. This WIPES the current PS5 save first, then uploads the backup — if the upload is interrupted (network drop, disk full), the save can be left empty with no automatic rollback. Back up the existing save first if it matters." +
            (entry.kind === "ps4"
              ? "\n\n⚠ PS4-format save: the PS4 emulator may not pick up the restored data immediately. If the save doesn't appear in-game after restore, close and reopen the game or restart the console."
              : ""),
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
      task = beginTask({ kind: "save-restore", origin: "saves", label: `Save restore ${entry.title_id}`, consoleId: restoreHost });
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
      const addr = consoleAddr(restoreHost);
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
          body:
            `Uploaded ${entry.title_id}.zip back to ${entry.path}.` +
            (entry.kind === "ps4"
              ? " (PS4 save — if it doesn't appear in-game, close and reopen the game or restart the console.)"
              : ""),
        },
      );
    } catch (e) {
      task?.fail(e);
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
      task?.done();
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
    const addr = consoleAddr(backupHost);
    const base = getSavePath();
    markBusy(entry.path, true);
    let tempDir: string | null = null;
    let task: TaskHandle | null = null;
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
      task = beginTask({ kind: "save-backup", origin: "saves", label: `Save backup ${entry.title_id} to USB`, consoleId: backupHost });
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
      // One folder per user (`<name>-<uid hex>`), so a shared console's users never restore
      // each other's backups of the same game (saveUsers.ts).
      const remoteDir = `${base}/${entry.title_id}/${usbUserDir(entry.user_id, ownerName(entry))}/${backupTimestamp()}`;
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
      task?.fail(e);
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification(
        "error",
        withConsolePrefix(backupHost, `USB backup failed: ${entry.title_id}`),
        { body: msg },
      );
      throw e; // let the bulk handler count this as a failure
    } finally {
      task?.done();
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
   * The newest USB backup of `entry`'s title for `entry`'s user, relative to
   * `<base>/<title_id>/` (saveUsers.ts latestUsbBackup). Null when there is none.
   */
  async function resolveLatestUsbBackup(
    addr: string,
    base: string,
    entry: SaveEntry,
  ): Promise<string | null> {
    const titleDir = `${base}/${entry.title_id}`;
    const onlyUser = (allSaves ?? [])
      .filter((s) => s.title_id === entry.title_id)
      .every((s) => userHex(s.user_id) === userHex(entry.user_id));
    try {
      return await latestUsbBackup(
        async (rel) => {
          const list = await fsListDir(addr, rel ? `${titleDir}/${rel}` : titleDir).catch(() => []);
          return list.filter((e) => e.kind === "dir").map((e) => e.name);
        },
        entry.user_id,
        onlyUser,
      );
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
    const addr = consoleAddr(restoreHost);
    const base = getSavePath();
    // Claim before any async work — same reasoning as handleRestore.
    markBusy(entry.path, true);
    let tempDir: string | null = null;
    let task: TaskHandle | null = null;
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
      const latestRel = await resolveLatestUsbBackup(addr, base, entry);
      if (!latestRel) return "no-backup";
      const latest = latestRel.split("/").pop() ?? latestRel;
      const zipName = `${entry.title_id}.zip`;
      const remoteZip = `${base}/${entry.title_id}/${latestRel}/${zipName}`;
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
      task = beginTask({ kind: "save-restore", origin: "saves", label: `Save restore ${entry.title_id} from USB`, consoleId: restoreHost });
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
      task?.fail(e);
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification(
        "error",
        withConsolePrefix(restoreHost, `USB restore failed: ${entry.title_id}`),
        { body: msg },
      );
      throw e; // let the bulk handler count this as a failure
    } finally {
      task?.done();
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
    const addr = consoleAddr(restoreHost);
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
    const addr = consoleAddr(backupHost);
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
    <div className="app-page">
      {confirmDialogNode}
      <PageHeader
        icon={Save}
        title={tr("saves_title", undefined, "Save data")}
        count={saves?.length}
        loading={loading}
        description={
          isAndroid()
            ? tr("saves_android_destination", undefined, "Backups are saved as dated ZIP files in this phone’s Download folder (/storage/emulated/0/Download). Restore copies encrypted files; it does not change the owning account or title ID.")
            : isTauriEnv()
            ? tr(
                "saves_description_v2",
                undefined,
                "Save data on the PS5, one entry per game. Back a game's saves up to a .zip on this computer and restore them later.",
              )
            : tr(
                "saves_description_browser",
                undefined,
                "Save data on the PS5, one entry per game. Backing saves up to a .zip and restoring them needs the desktop app; here you can see them and open their folders.",
              )
        }
        right={
          <div className="flex items-center gap-2">
            {isTauriEnv() && (
              <>
                <Button
                  variant="secondary"
                  size="sm"
                  leftIcon={
                    bulkBackupBusy ? (
                      <Spinner size={12} tone="inherit" />
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
                      <Spinner size={12} tone="inherit" />
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
              </>
            )}
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
        <AutoBackupCard />
        <ServerBackupCard owners={owners} />
        <MoveFromPs4 />
        {error && (
          <div className="mb-4">
            <ErrorCard
              title={tr("saves_error", undefined, "Couldn't list saves")}
              detail={error}
              onRetry={() => void refresh()}
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
              <Button variant="secondary" size="md" onClick={() => navigate("/games")}>
                {tr("saves_empty_cta", "Browse Library")}
              </Button>
            }
          />
        )}

        <div className="space-y-3">
          {saves?.some((e) => e.kind === "ps4") && (
            <div className="rounded-[var(--radius-card)] border border-[var(--color-warn)]/40 bg-[var(--color-warn)]/10 p-3 text-xs text-[var(--color-warn)]">
              <strong>
                {tr("saves_ps4_warning_title", undefined, "PS4-format saves:")}
              </strong>{" "}
              {tr(
                "saves_ps4_warning_body",
                undefined,
                "PS4-format save restore has unresolved reliability reports. Restarting the game or console is not a verified fix. Restore does not re-sign saves or change their account or title ID. Keep a backup of the target save before restoring; see the FAQ for diagnostic details.",
              )}
            </div>
          )}
          {openSave?.mounted && (
            <div
              className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-accent-soft)] p-3 text-xs"
              data-testid="save-open-banner"
            >
              <LockOpen size={14} className="shrink-0 text-[var(--color-accent-bright)]" aria-hidden />
              <span className="min-w-0 flex-1">
                {tr(
                  "saves_open_banner",
                  { name: openSave.image.split("/").pop() ?? openSave.image, path: openSave.mount_point },
                  "{name} is open at {path}. Changes are written back into the save when you close it.",
                )}
              </span>
              <Button size="sm" variant="secondary" leftIcon={<FolderOpen size={11} />} onClick={() => openInFileSystem(navigate, openSave.mount_point)}>
                {tr("saves_open_in_files", undefined, "Open in Files")}
              </Button>
              <Button size="sm" variant="primary" leftIcon={<Lock size={11} />} disabled={openBusy} onClick={() => void handleCloseSave()}>
                {tr("saves_close", undefined, "Close & write back")}
              </Button>
            </div>
          )}
          <input
            ref={shareInput}
            type="file"
            accept=".zip,application/zip"
            className="hidden"
            data-testid="save-use-shared-file"
            onChange={(ev) => {
              const f = ev.target.files?.[0];
              ev.target.value = "";
              if (f) void doUseShared(f);
              else setUseTarget(null);
            }}
          />
          {picker && (
            <Modal
              open
              onClose={() => setPicker(null)}
              title={tr("saves_open_pick_title", { title: picker.entry.title_id }, "Which save of {title}?")}
              size="md"
            >
              <ul className="flex flex-col gap-1" data-testid="save-open-picker">
                {picker.images.map((img) => (
                  <li key={img}>
                    <button
                      type="button"
                      className="w-full rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-left font-mono text-xs hover:bg-[var(--color-surface-raised)]"
                      onClick={() => {
                        setPicker(null);
                        void runOn(picker.entry, img, picker.then);
                      }}
                    >
                      {img.split("/").pop()}
                    </button>
                  </li>
                ))}
              </ul>
            </Modal>
          )}
          {owners.length > 1 && (
            <div className="max-w-xs" data-testid="saves-user-filter">
              <Select
                label={tr("saves_user_filter", undefined, "Whose saves")}
                value={userFilter}
                onChange={(ev) => setUserFilter(ev.target.value)}
                options={[
                  { value: "all", label: tr("saves_user_all", undefined, "All users") },
                  ...owners.map((o) => ({
                    value: userHex(o.id),
                    label: o.name ?? tr("saves_user_other", { id: userHex(o.id) }, "Other user ({id})"),
                  })),
                ]}
              />
            </div>
          )}
          <div className="card-flow">
          {grouped.map(({ title_id, entries }) => (
            <section
              key={title_id}
              data-title-id={title_id}
              className={
                "surface-panel p-5" +
                (focusTitle && title_id.toUpperCase() === focusTitle
                  ? " ring-2 ring-[var(--color-accent)]"
                  : "")
              }
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
                    className="flex items-center gap-3 rounded-[var(--radius-field)] bg-[var(--color-surface)] px-2 py-1.5 text-xs"
                  >
                    <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-xs uppercase">
                      {e.kind}
                    </span>
                    <div className="min-w-0 flex-1">
                      <code className="block truncate text-xs text-[var(--color-muted)]">
                        {e.path}
                      </code>
                      <div className="text-xs text-[var(--color-muted)]">
                        {ownerName(e) ??
                          tr("saves_user_other", { id: userHex(e.user_id) }, "Other user ({id})")}{" "}
                        ·{" "}
                        {formatBytes(e.size)} ·{" "}
                        {formatDate(e.mtime * 1000, "date")}
                      </div>
                    </div>
                    <Button
                      variant="secondary"
                      size="sm"
                      leftIcon={<LockOpen size={11} />}
                      onClick={() => void handleOpenSave(e)}
                      disabled={isBusy(e.path) || openBusy}
                      title={tr(
                        "saves_open_hint",
                        undefined,
                        "Mount this save decrypted on the PS5 and browse its files in Files. Changes are written back when you close it.",
                      )}
                    >
                      {tr("saves_open", undefined, "Open save")}
                    </Button>
                    {e.kind === "ps5" && (
                      <>
                        <Button
                          variant="ghost"
                          size="sm"
                          leftIcon={<Share2 size={11} />}
                          onClick={() => void handleOpenSave(e, "share")}
                          disabled={isBusy(e.path) || openBusy}
                          title={tr(
                            "saves_share_hint",
                            undefined,
                            "Save this save's files as a .zip to give to a friend: it loads on their PS5 under their own account (Use a shared save).",
                          )}
                          data-testid="save-share"
                        >
                          {tr("saves_share", undefined, "Share")}
                        </Button>
                        <Button
                          variant="ghost"
                          size="sm"
                          leftIcon={<FileDown size={11} />}
                          onClick={() => void handleOpenSave(e, "use")}
                          disabled={isBusy(e.path) || openBusy}
                          title={tr(
                            "saves_use_shared_hint",
                            undefined,
                            "Put a friend's shared save (.zip) into your save of this game. It becomes yours: no account ID change needed.",
                          )}
                          data-testid="save-use-shared"
                        >
                          {tr("saves_use_shared", undefined, "Use a shared save")}
                        </Button>
                      </>
                    )}
                    {isTauriEnv() && (
                      <>
                        <Button
                          variant="ghost"
                          size="sm"
                          leftIcon={<Download size={11} />}
                          onClick={() => handleDownload(e)}
                          disabled={isBusy(e.path)}
                        >
                          {tr("saves_download", undefined, "Backup")}
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
                        {/* Both USB actions in one menu: four buttons a row was too many. */}
                        <OverflowMenu
                          triggerLabel={tr("saves_usb_menu", undefined, "USB")}
                          triggerIcon={<HardDrive size={11} />}
                          ariaLabel={tr("saves_usb_menu_aria", undefined, "USB backup actions")}
                          buttonTitle={tr("saves_usb_menu_aria", undefined, "USB backup actions")}
                          items={[
                            {
                              label: tr("saves_backup_usb", undefined, "Save to USB"),
                              icon: <HardDrive size={12} />,
                              onSelect: () => handleBackupToUsb(e),
                              disabled: isBusy(e.path) || bulkBackupBusy,
                              title: tr(
                                "saves_backup_usb_tooltip_v2",
                                undefined,
                                "Zip this save onto the drive at the save path set in Settings (a USB drive on the PS5). It is copied to this computer, zipped and sent back, so keep the app open; the copy here is deleted afterwards.",
                              ),
                            },
                            {
                              label: tr("saves_restore_usb", undefined, "Restore from USB"),
                              icon: <UploadIcon size={12} />,
                              onSelect: () => handleRestoreFromUsb(e),
                              disabled: isBusy(e.path) || bulkRestoreBusy,
                              destructive: true,
                              title: tr(
                                "saves_restore_usb_tooltip_v2",
                                undefined,
                                "Restore this save from its latest backup at the save path set in Settings. The zip is copied to this computer, unpacked and sent back to the PS5, so keep the app open. Overwrites the live save.",
                              ),
                            },
                          ]}
                        />
                      </>
                    )}
                  </li>
                ))}
              </ul>
            </section>
          ))}
          </div>
          {payloadStatus === "up" && host?.trim() && (
            <SwitchSaves host={host} refreshKey={refreshCount} />
          )}
        </div>

        {loading && saves === null && (
          <div className="mt-4 text-center text-xs text-[var(--color-muted)]">
            <Spinner size={12} className="mr-2 inline" />
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
  // A backup folder (`PPSA17221.bak`) is named after its game: show that game's name.
  const info = useTitleInfo(iconTitleId(titleId) ?? titleId);
  // The game's page, when the folder names a game (a backup's `.bak` names one too).
  const gameId = (iconTitleId(titleId) ?? titleId).toUpperCase();
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
        {hasTitleId(gameId) ? (
          <Link to={gamePath(gameId)} className="hover:underline">
            {info?.title ? `${info.title} (${titleId})` : titleId}
          </Link>
        ) : info?.title ? (
          `${info.title} (${titleId})`
        ) : (
          titleId
        )}
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
