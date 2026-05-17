import { useCallback, useEffect, useMemo, useState } from "react";
// useEffect is consumed inside SaveThumbnail below.
import {
  Save,
  RefreshCw,
  Loader2,
  Download,
  HardDrive,
  Upload as UploadIcon,
} from "lucide-react";
import {
  savesList,
  startTransferDir,
  fsReadPreview,
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
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { PageHeader, Button, EmptyState, ErrorCard } from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { startTransferDownload } from "../../api/ps5";
import { formatBytes } from "../../lib/format";
import { mgmtAddr } from "../../lib/addr";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { pushNotification } from "../../state/notifications";

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
  const isBusy = useCallback(
    (path: string) => busy.has(path),
    [busy],
  );
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
      const destZip = await saveDialog({
        defaultPath: `${entry.title_id}.zip`,
        filters: [{ name: "ZIP archive", extensions: ["zip"] }],
        title: tr(
          "saves_download_picker",
          undefined,
          "Save backup as…",
        ),
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
      await saveArchiveZip(tempDir, entry.title_id, destZip);
      pushNotification("success", `Backed up ${entry.title_id}`, {
        body: `Saved to ${destZip}`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification("error", `Backup failed: ${entry.title_id}`, { body: msg });
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
          "Pick a .zip whose top-level folder is named the same as the title ID. The current PS5 save will be overwritten.",
        ),
        destructive: true,
        confirmLabel: tr("saves_restore_confirm_label", undefined, "Restore"),
      });
      if (!ok) return;

      const localZip = await openDialog({
        multiple: false,
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
      pushNotification("success", `Restored ${entry.title_id}`, {
        body: `Uploaded ${entry.title_id}.zip back to ${entry.path}.`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification("error", `Restore failed: ${entry.title_id}`, { body: msg });
    } finally {
      if (tempDir) await saveArchiveCleanupTemp(tempDir).catch(() => {});
      markBusy(entry.path, false);
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
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={refresh}
            disabled={loading || !host?.trim() || payloadStatus !== "up"}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        }
      />

      {payloadStatus !== "up" && (
        <EmptyState
          icon={Save}
          message={tr(
            "saves_no_payload",
            undefined,
            "Connect to your PS5 first — saves are read from the live console.",
          )}
        />
      )}

      {error && (
        <div className="mb-4">
          <ErrorCard
            title={tr("saves_error", undefined, "Couldn't list saves")}
            detail={error}
          />
        </div>
      )}

      {saves && saves.length === 0 && payloadStatus === "up" && (
        <EmptyState
          icon={Save}
          message={tr(
            "saves_empty",
            undefined,
            "No saves found. Either no users have ever saved a game, or the savedata folders aren't where we expect them.",
          )}
        />
      )}

      <div className="mx-auto max-w-4xl space-y-3">
        {grouped.map(({ title_id, entries }) => (
          <section
            key={title_id}
            className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
          >
            <header className="mb-2 flex items-center gap-2">
              <SaveThumbnail
                titleId={title_id}
                userId={entries[0]?.user_id ?? 0}
                mgmtAddr={host?.trim() ? `${host.trim()}:9114` : null}
              />
              <h3 className="text-sm font-semibold">{title_id}</h3>
              <span className="text-[11px] text-[var(--color-muted)]">
                {entries.length} {tr("saves_folder", undefined, "folder")}
                {entries.length === 1 ? "" : "s"}
              </span>
            </header>
            <ul className="space-y-1">
              {entries.map((e) => (
                <li
                  key={e.path}
                  className="flex items-center gap-3 rounded-md bg-[var(--color-surface)] px-2 py-1.5 text-xs"
                >
                  <span className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5 text-[10px] uppercase">
                    {e.kind}
                  </span>
                  <div className="min-w-0 flex-1">
                    <code className="block truncate text-[10px] text-[var(--color-muted)]">
                      {e.path}
                    </code>
                    <div className="text-[10px] text-[var(--color-muted)]">
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
    </div>
  );
}

// formatBytes moved to lib/format.ts (and corrected to IEC binary).

/**
 * Per-save thumbnail. Lazy-loads icon0.png from the save folder's
 * sce_sys/. The full path Sony uses is
 *   /user/home/<uid>/savedata_prospero/<title_id>/sce_sys/icon0.png
 * IntersectionObserver gates the FS_READ so a Saves screen with
 * 200+ entries doesn't fan out 200+ concurrent reads on mount.
 *
 * Falls back to the HardDrive icon when icon0.png is missing
 * (PS4 saves often don't have one).
 */
function SaveThumbnail({
  titleId,
  userId,
  mgmtAddr,
}: {
  titleId: string;
  userId: number;
  mgmtAddr: string | null;
}) {
  const [src, setSrc] = useState<string | null>(null);
  const [tried, setTried] = useState(false);
  const ref = useState<HTMLDivElement | null>(null);
  const [refEl, setRefEl] = useState<HTMLDivElement | null>(null);
  // Keep the unused ref tuple slot quiet without triggering the
  // unused-var lint — we only need the setter.
  void ref;

  useEffect(() => {
    if (tried || !refEl || !mgmtAddr) return;
    const obs = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          obs.disconnect();
          setTried(true);
          const path = `/user/home/${userId}/savedata_prospero/${titleId}/sce_sys/icon0.png`;
          fsReadPreview(mgmtAddr, path, 64 * 1024)
            .then((r) => {
              if (r.size > 0) {
                setSrc(`data:image/png;base64,${r.base64}`);
              }
            })
            .catch(() => {
              // Most saves don't have an icon — silently no-op.
            });
          break;
        }
      }
    });
    obs.observe(refEl);
    return () => obs.disconnect();
  }, [tried, refEl, mgmtAddr, titleId, userId]);

  if (src) {
    return (
      <img
        src={src}
        alt={titleId}
        className="h-8 w-8 shrink-0 rounded object-cover"
      />
    );
  }
  return (
    <div
      ref={setRefEl}
      className="flex h-8 w-8 shrink-0 items-center justify-center rounded bg-[var(--color-surface)]"
    >
      <HardDrive size={14} className="text-[var(--color-muted)]" />
    </div>
  );
}
