import { useCallback, useEffect, useRef, useState } from "react";
import {
  Archive,
  RefreshCw,
  RotateCcw,
  Trash2,
  Plus,
  AlertTriangle,
  Clock,
  HardDrive,
} from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
  EmptyState,
  Card,
  Spinner,
  Input,
} from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import {
  backupSnapshot,
  backupList,
  backupRestore,
  backupDelete,
  type BackupEntry,
} from "../../api/ps5";
import { trackTask } from "../../state/trackTask";
import { humanizePs5Error } from "../../lib/humanizeError";

function formatTimestamp(ts: number): string {
  const d = new Date(ts * 1000);
  return d.toLocaleString();
}

/** Things people actually want to keep a copy of.
 *
 *  Deliberately conservative: user data the helper or homebrew owns, plus
 *  the home-screen tile metadata that goes wrong often enough to be worth
 *  saving. System paths are not offered — a snapshot is only as safe as
 *  the restore, and restoring over live system state is not something to
 *  suggest with a one-click button. */
const BACKUP_PRESETS: {
  path: string;
  label: string;
  labelKey: string;
}[] = [
  { path: "/data/homebrew", label: "Homebrew apps", labelKey: "backup_preset_homebrew" },
  { path: "/user/appmeta", label: "Game tiles & icons", labelKey: "backup_preset_appmeta" },
  { path: "/data/ps5upload/cheats", label: "Cheats", labelKey: "backup_preset_cheats" },
  { path: "/data/ps5upload/patches", label: "Patches", labelKey: "backup_preset_patches" },
];

/** A name that reads like something, derived from the path and today's date.
 *
 *  The old field just said "Tag (alphanumeric, dash, underscore)" and left
 *  it blank, so the rules were stated but the answer was not. */
function defaultTagFor(p: string): string {
  const leaf =
    p
      .split("/")
      .filter(Boolean)
      .pop()
      ?.replace(/[^A-Za-z0-9_-]/g, "-") ?? "snapshot";
  const d = new Date();
  const stamp = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  return `${leaf}-${stamp}`.slice(0, 32);
}

export default function BackupScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const { confirm, dialog: confirmDialog } = useConfirm();

  const [entries, setEntries] = useState<BackupEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [tag, setTag] = useState("");
  /* Once the user names a snapshot themselves, stop rewriting it. */
  const [tagTouched, setTagTouched] = useState(false);
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState(false);
  const [actionMsg, setActionMsg] = useState<string | null>(null);

  const reqIdRef = useRef(0);

  useEffect(() => {
    const id = reqIdRef.current;
    return () => {
      reqIdRef.current = id + 1;
    };
  }, []);

  const refresh = useCallback(async () => {
    if (!addr) return;
    if (payloadStatus !== "up") return;
    const myId = ++reqIdRef.current;
    setLoading(true);
    setError(null);
    try {
      const list = await backupList(addr);
      if (myId !== reqIdRef.current) return;
      setEntries(list.snapshots);
    } catch (e) {
      if (myId !== reqIdRef.current) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      if (myId === reqIdRef.current) setLoading(false);
    }
  }, [addr, payloadStatus]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleSnapshot = useCallback(async () => {
    if (!addr || !tag.trim() || !path.trim()) return;
    setBusy(true);
    setActionMsg(null);
    setError(null);
    try {
      const result = await trackTask(
        { kind: "backup-snapshot", origin: "backup", label: `Backup ${tag.trim()}`, consoleId: addr },
        () => backupSnapshot(tag.trim(), path.trim(), addr),
      );
      setActionMsg(
        tr(
          "backup_snapshot_ok",
          { files: result.files, bytes: formatBytes(result.bytes) },
          `Snapshotted ${result.files} files (${formatBytes(result.bytes)})`,
        ),
      );
      setTag("");
      setPath("");
      refresh();
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setBusy(false);
    }
  }, [addr, tag, path, tr, refresh]);

  const handleRestore = useCallback(
    async (entry: BackupEntry) => {
      if (!addr) return;
      const ok = await confirm({
        title: tr("backup_restore_confirm_title", undefined, "Restore snapshot?"),
        message: tr(
          "backup_restore_confirm_msg",
          { tag: entry.tag, ts: formatTimestamp(entry.timestamp) },
          `Restore ${entry.tag} @ ${formatTimestamp(entry.timestamp)}? This will overwrite current files.`,
        ),
        confirmLabel: tr("backup_restore", undefined, "Restore"),
        destructive: true,
      });
      if (!ok) return;
      setBusy(true);
      setActionMsg(null);
      setError(null);
      try {
        const result = await trackTask(
          { kind: "backup-restore", origin: "backup", label: `Restore ${entry.tag}`, consoleId: addr },
          () => backupRestore(entry.tag, entry.timestamp, addr),
        );
        setActionMsg(
          tr(
            "backup_restore_ok",
            { count: result.restored },
            `Restored ${result.restored} files`,
          ),
        );
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      } finally {
        setBusy(false);
      }
    },
    [addr, confirm, tr],
  );

  const handleDelete = useCallback(
    async (entry: BackupEntry) => {
      if (!addr) return;
      const ok = await confirm({
        title: tr("backup_delete_confirm_title", undefined, "Delete snapshot?"),
        message: tr(
          "backup_delete_confirm_msg",
          { tag: entry.tag, ts: formatTimestamp(entry.timestamp) },
          `Delete ${entry.tag} @ ${formatTimestamp(entry.timestamp)}? This cannot be undone.`,
        ),
        confirmLabel: tr("backup_delete", undefined, "Delete"),
        destructive: true,
      });
      if (!ok) return;
      setBusy(true);
      setActionMsg(null);
      setError(null);
      try {
        await backupDelete(entry.tag, entry.timestamp, addr);
        setActionMsg(tr("backup_delete_ok", undefined, "Snapshot deleted"));
        refresh();
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      } finally {
        setBusy(false);
      }
    },
    [addr, confirm, tr, refresh],
  );

  return (
    <div className="p-6">
      <div className="mx-auto max-w-4xl space-y-4">
        <PageHeader
          icon={Archive}
          title={tr("backup_title", undefined, "Backup & Restore")}
          description={tr(
            "backup_subtitle",
            undefined,
            "Snapshot files on the PS5 and restore them later",
          )}
          right={
            <Button
              variant="ghost"
              size="sm"
              onClick={refresh}
              disabled={loading || busy}
            >
              {loading ? (
                <Spinner size={16} tone="inherit" />
              ) : (
                <RefreshCw size={16} />
              )}
            </Button>
          }
        />

        <ConnectionGate>
          {error && <div className="mb-4"><ErrorCard title={error} /></div>}

          {actionMsg && (
            <div className="rounded-lg border border-[var(--color-good)]/30 bg-[var(--color-good)]/10 px-4 py-3 text-sm text-[var(--color-good)]">
              {actionMsg}
            </div>
          )}

          <Card className="space-y-4">
            <h3 className="flex items-center gap-2 text-sm font-medium">
              <Plus size={16} />
              {tr("backup_new_snapshot", undefined, "New snapshot")}
            </h3>

            {/* Say what this is for. The screen used to open with two empty
                boxes labelled "Tag" and "PS5 path", which assumes the reader
                already knows both what to copy and what to call it. */}
            <p className="text-sm text-[var(--color-muted)]">
              {tr(
                "backup_explain",
                undefined,
                "Copies files from the console to this computer so you can put them back later. Pick something common below, or type your own path.",
              )}
            </p>

            {/* Common targets, so the first action is a click and not a
                guess. Choosing one fills in a sensible tag too. */}
            <div className="flex flex-wrap gap-2">
              {BACKUP_PRESETS.map((preset) => (
                <button
                  key={preset.path}
                  type="button"
                  onClick={() => {
                    setPath(preset.path);
                    setTag(defaultTagFor(preset.path));
                  }}
                  title={preset.path}
                  className={`rounded-md border px-2 py-1 text-xs ${
                    path === preset.path
                      ? "border-[var(--color-accent)] bg-[var(--color-accent)]/10"
                      : "border-[var(--color-border)] hover:bg-[var(--color-surface)]"
                  }`}
                >
                  {tr(preset.labelKey, undefined, preset.label)}
                </button>
              ))}
            </div>

            <div className="space-y-3">
              <Input
                label={tr("backup_path_label", undefined, "What to copy")}
                type="text"
                value={path}
                onChange={(e) => {
                  setPath(e.target.value);
                  /* Keep the tag in step while it is still auto-derived, so
                     the common case needs no thought — but never clobber a
                     name the user typed themselves. */
                  if (!tagTouched) setTag(defaultTagFor(e.target.value));
                }}
                placeholder="/data/homebrew"
                className="font-mono"
              />
              <Input
                label={tr("backup_tag_label", undefined, "Name for this snapshot")}
                type="text"
                value={tag}
                onChange={(e) => {
                  setTagTouched(true);
                  setTag(e.target.value);
                }}
                maxLength={32}
                placeholder={tr("backup_tag_placeholder", undefined, "homebrew-2026-08-20")}
              />
              <Button
                onClick={handleSnapshot}
                disabled={busy || !tag.trim() || !path.trim()}
                size="sm"
              >
                {busy ? (
                  <Spinner size={16} tone="inherit" />
                ) : (
                  <Archive size={16} />
                )}
                {tr("backup_create", undefined, "Create snapshot")}
              </Button>
            </div>
          </Card>

          <Card className="space-y-3">
            <h3 className="flex items-center gap-2 text-sm font-medium">
              <Clock size={16} />
              {tr("backup_snapshots", undefined, "Snapshots")}
            </h3>

            {entries.length === 0 && !loading ? (
              <EmptyState
                icon={Archive}
                title={tr("backup_empty", undefined, "No snapshots yet")}
                message={tr(
                  "backup_empty_hint",
                  undefined,
                  "Create a snapshot above to get started",
                )}
              />
            ) : (
              <div className="space-y-2">
                {entries.map((entry) => (
                  <div
                    key={`${entry.tag}-${entry.timestamp}`}
                    className="flex items-center justify-between rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 py-2.5"
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-2">
                        <span className="badge badge-sm">
                          {entry.tag}
                        </span>
                        <span className="text-xs text-[var(--color-muted)]">
                          {formatTimestamp(entry.timestamp)}
                        </span>
                      </div>
                      <div className="mt-1 flex items-center gap-3 text-xs text-[var(--color-muted)]">
                        <span className="flex items-center gap-1">
                          <HardDrive size={12} />
                          {entry.files} {tr("backup_files_unit", undefined, "files")}
                        </span>
                        <span>{formatBytes(entry.bytes)}</span>
                      </div>
                    </div>
                    <div className="flex items-center gap-1">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleRestore(entry)}
                        disabled={busy}
                        title={tr("backup_restore", undefined, "Restore")}
                      >
                        <RotateCcw size={14} />
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => handleDelete(entry)}
                        disabled={busy}
                        title={tr("backup_delete", undefined, "Delete")}
                        className="text-[var(--color-bad)] hover:text-[var(--color-bad)]"
                      >
                        <Trash2 size={14} />
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </Card>

          {busy && (
            <div className="flex items-center justify-center gap-2 py-4 text-sm text-[var(--color-muted)]">
              <Spinner size={16} />
              {tr("backup_working", undefined, "Working...")}
            </div>
          )}

          <div className="flex items-start gap-2 rounded-lg border border-[var(--color-warn)]/20 bg-[var(--color-warn)]/5 px-4 py-3 text-xs text-[var(--color-warn)]">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" />
            <span>
              {tr(
                "backup_warning",
                undefined,
                "Snapshots are stored on the PS5 at /data/ps5upload/backups/. Restore overwrites existing files at their original paths.",
              )}
            </span>
          </div>
        </ConnectionGate>
        {confirmDialog}
      </div>
    </div>
  );
}
