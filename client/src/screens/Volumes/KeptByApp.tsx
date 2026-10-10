import { useCallback, useEffect, useState } from "react";
import { FolderOpen, Trash2 } from "lucide-react";

import { fsDelete } from "../../api/ps5";
import { Button, Spinner } from "../../components";
import { transferAddr } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { invoke } from "../../lib/invokeLogged";
import { ps5uploadUsage, type DirEntry, type KeptKey, type UsageRow } from "../../lib/ps5uploadUsage";
import { useTr } from "../../state/lang";

/** The helper answers at most this many entries per call. */
const LIST_PAGE = 256;

/** One folder, listed completely (the helper pages at 256 entries). */
async function listDirAll(addr: string, path: string): Promise<DirEntry[]> {
  const all: DirEntry[] = [];
  let offset = 0;
  for (;;) {
    const res = await invoke<{ entries?: DirEntry[]; truncated?: boolean }>("ps5_list_dir", {
      addr,
      path,
      offset,
      limit: LIST_PAGE,
    });
    const ents = res.entries ?? [];
    all.push(...ents);
    offset += ents.length;
    if (ents.length === 0 || (!res.truncated && ents.length < LIST_PAGE)) break;
  }
  return all;
}

export function KeptByAppView({
  rows,
  busyPath,
  onClean,
  onOpen,
}: {
  /** null while it is still measuring. */
  rows: UsageRow[] | null;
  /** The folder being emptied right now. */
  busyPath: string | null;
  onClean: (row: UsageRow) => void;
  onOpen: (row: UsageRow) => void;
}) {
  const tr = useTr();
  const label = (key: KeptKey) =>
    key === "pkg_library"
      ? tr("kept_pkg_library", undefined, "Package library")
      : key === "pkg_temp"
        ? tr("kept_pkg_temp", undefined, "Package temp files")
        : key === "backups"
          ? tr("kept_backups", undefined, "Save backups")
          : tr("kept_tests", undefined, "Test files");
  return (
    <section className="mt-6" data-testid="kept-by-app">
      <div className="mb-1 text-sm font-semibold">
        {tr("kept_title", undefined, "Kept by ps5upload")}
      </div>
      <p className="mb-2 text-xs text-[var(--color-muted)]">
        {tr(
          "kept_intro",
          undefined,
          "The folders ps5upload itself writes on this console, and how much each holds. Temp and test files can be cleaned up; your packages and save backups are only opened from here, never deleted.",
        )}
      </p>
      {rows === null ? (
        <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Spinner size={12} />
          {tr("kept_measuring", undefined, "Measuring…")}
        </div>
      ) : rows.length === 0 ? (
        <div className="text-xs text-[var(--color-muted)]">
          {tr("kept_nothing", undefined, "ps5upload is not keeping anything on this console.")}
        </div>
      ) : (
        <ul className="rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] divide-y divide-[var(--color-border)]">
          {rows.map((r) => (
            <li key={r.path} className="flex flex-wrap items-center gap-2 px-3 py-2 text-sm">
              <div className="min-w-0 flex-1">
                <div className="font-medium">
                  {label(r.key)}
                  <span className="ml-2 font-mono text-xs text-[var(--color-muted)]">{r.drive}</span>
                </div>
                <div className="truncate font-mono text-xs text-[var(--color-muted)]">{r.path}</div>
              </div>
              <div className="text-right text-xs text-[var(--color-muted)]">
                <div className="font-medium text-[var(--color-text)]">
                  {r.truncated
                    ? tr("kept_at_least", { size: formatBytes(r.bytes) }, "at least {size}")
                    : formatBytes(r.bytes)}
                </div>
                <div>{tr("kept_files", { count: r.files }, "{count} files")}</div>
              </div>
              {r.cleanable ? (
                <Button
                  size="sm"
                  variant="secondary"
                  loading={busyPath === r.path}
                  disabled={busyPath !== null}
                  leftIcon={<Trash2 size={14} />}
                  onClick={() => onClean(r)}
                  data-testid={`kept-clean-${r.path}`}
                >
                  {tr("kept_clean", undefined, "Clean up")}
                </Button>
              ) : (
                <Button
                  size="sm"
                  variant="ghost"
                  leftIcon={<FolderOpen size={14} />}
                  onClick={() => onOpen(r)}
                  data-testid={`kept-open-${r.path}`}
                >
                  {tr("kept_open", undefined, "Open in Files")}
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Measures what ps5upload keeps on `drives` and offers to clean up the disposable part.
 *  `refreshKey` changes when the drive list was reloaded; `confirm` is the screen's dialog. */
export function KeptByApp({
  host,
  drives,
  refreshKey,
  confirm,
  onOpenPath,
  onChanged,
}: {
  host: string;
  drives: string[];
  refreshKey: number;
  confirm: (opts: { title: string; message: string; confirmLabel: string; destructive: boolean }) => Promise<boolean>;
  onOpenPath: (path: string) => void;
  onChanged: () => void;
}) {
  const tr = useTr();
  const [rows, setRows] = useState<UsageRow[] | null>(null);
  const [busyPath, setBusyPath] = useState<string | null>(null);
  const [round, setRound] = useState(0);
  const key = drives.join("|");

  useEffect(() => {
    let stale = false;
    const addr = transferAddr(host);
    void ps5uploadUsage(key ? key.split("|") : [], (path) => listDirAll(addr, path))
      .then((r) => {
        if (!stale) setRows(r);
      })
      .catch(() => {
        if (!stale) setRows([]);
      });
    return () => {
      stale = true;
    };
  }, [host, key, refreshKey, round]);

  const clean = useCallback(
    async (row: UsageRow) => {
      const ok = await confirm({
        title: tr("kept_clean_title", undefined, "Delete these files from the PS5?"),
        message: tr(
          "kept_clean_body",
          { path: row.path, size: formatBytes(row.bytes) },
          "Everything in {path} ({size}) will be deleted. These are leftovers ps5upload can recreate; nothing in your package library or save backups is touched.",
        ),
        confirmLabel: tr("kept_clean", undefined, "Clean up"),
        destructive: true,
      });
      if (!ok) return;
      setBusyPath(row.path);
      try {
        await fsDelete(transferAddr(host), row.path);
      } finally {
        setBusyPath(null);
        setRound((n) => n + 1);
        onChanged();
      }
    },
    [confirm, host, onChanged, tr],
  );

  return (
    <KeptByAppView
      rows={rows}
      busyPath={busyPath}
      onClean={(r) => void clean(r)}
      onOpen={(r) => onOpenPath(r.path)}
    />
  );
}
