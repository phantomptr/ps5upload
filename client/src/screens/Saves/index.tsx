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
  waitForJob,
  type SaveEntry,
} from "../../api/ps5";
import { useConnectionStore, PS5_PAYLOAD_PORT } from "../../state/connection";
import { PageHeader, Button, EmptyState, ErrorCard } from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { startTransferDownload } from "../../api/ps5";
import { formatBytes } from "../../lib/format";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
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
  const [saves, setSaves] = useState<SaveEntry[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();

  const refresh = useCallback(async () => {
    if (!host?.trim() || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      const r = await savesList(`${host.trim()}:9114`);
      setSaves(r.saves);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [host, payloadStatus]);

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
    const dest = await openDialog({
      directory: true,
      title: tr(
        "saves_download_picker",
        undefined,
        "Pick a folder to save the backup into",
      ),
    });
    if (!dest || typeof dest !== "string") return;
    try {
      await startTransferDownload(
        entry.path,
        dest,
        `${host.trim()}:${PS5_PAYLOAD_PORT}`,
        "folder",
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  async function handleRestore(entry: SaveEntry) {
    if (!host?.trim()) return;
    const ok = await confirmDialog({
      title: tr(
        "saves_restore_confirm_title",
        { title: entry.title_id },
        `Restore ${entry.title_id} from local backup?`,
      ),
      message: tr(
        "saves_restore_confirm_body",
        undefined,
        "Pick the LOCAL backup folder you want to restore from. The current PS5 save will be overwritten.",
      ),
      destructive: true,
      confirmLabel: tr("saves_restore_confirm_label", undefined, "Restore"),
    });
    if (!ok) return;
    const localFolder = await openDialog({
      directory: true,
      title: tr(
        "saves_restore_picker",
        undefined,
        "Pick the local backup folder to restore from",
      ),
    });
    if (!localFolder || typeof localFolder !== "string") return;
    try {
      const addr = `${host.trim()}:${PS5_PAYLOAD_PORT}`;
      await fsDelete(addr, entry.path);
      const jobId = await startTransferDir(
        localFolder,
        entry.path,
        addr,
        null,
        [],
      );
      await waitForJob(jobId);
      pushNotification("success", `Restored ${entry.title_id}`, {
        body: `Uploaded backup to ${entry.path}.`,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      pushNotification("error", `Restore failed: ${entry.title_id}`, { body: msg });
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
          "Per-game save folders on the PS5. PS5 saves under savedata_prospero/, PS4 legacy saves under savedata/. Download a save folder to back it up; restore it later by uploading the same folder back.",
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
                {entries.length} folder
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
                      user {e.user_id} · {formatBytes(e.size)} ·{" "}
                      {new Date(e.mtime * 1000).toLocaleDateString()}
                    </div>
                  </div>
                  <Button
                    variant="ghost"
                    size="sm"
                    leftIcon={<Download size={11} />}
                    onClick={() => handleDownload(e)}
                  >
                    {tr("saves_download", undefined, "Backup")}
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    leftIcon={<UploadIcon size={11} />}
                    onClick={() => handleRestore(e)}
                    title={tr(
                      "saves_restore_tooltip",
                      undefined,
                      "Pick a local backup folder and upload it back to this save's PS5 path. Overwrites the live save.",
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
