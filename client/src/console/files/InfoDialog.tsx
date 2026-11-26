import { useEffect, useRef, useState } from "react";
import { Calculator, ShieldCheck, Square } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { call } from "../api";
import { getLanguage, t } from "../strings";
import { formatDateTime } from "../time";
import { topModal } from "../nav";
import { Modal } from "../ui/Modal";
import { refocusIfLost } from "../ui/restoreFocus";
import { startFolderSize, SIZE_MAX_ENTRIES, SIZE_MAX_MS, type SizeCount, type SizeResult } from "./folderSize";
import { LIST_PAGE } from "./useDir";
import { modeLetters, modeText } from "./permissions";
import { PermissionsDialog } from "./PermissionsDialog";

interface Stat {
  kind: string;
  size: number;
  mtime: number;
  mode: number;
}

type Info = { stat: Stat; items: string | null } | { error: true } | null;

function kindText(kind: string): string {
  if (kind === "dir") return t("cw_files_kind_dir");
  if (kind === "link") return t("cw_files_kind_link");
  return t("cw_files_kind_file");
}

type Walk = { state: "running"; count: SizeCount } | { state: "ended"; result: SizeResult } | { state: "held" } | null;

const num = (n: number) => n.toLocaleString(getLanguage());

/** "12,345 files · 67 folders · 8.1 GiB". */
export function countText(c: SizeCount): string {
  return t("cw_files_size_count", { files: num(c.files), folders: num(c.folders), size: formatBytes(c.bytes) });
}

/** What a walk that ended says besides the count (partial, cancelled, failed), or null. */
export function walkNote(r: SizeResult): string | null {
  let note: string | null = null;
  if (r.end === "capped") note = t("cw_files_size_capped", { n: num(SIZE_MAX_ENTRIES) });
  else if (r.end === "timeout") note = t("cw_files_size_timeout", { s: Math.round(SIZE_MAX_MS / 1000) });
  else if (r.end === "cancelled") note = t("cw_files_size_cancelled");
  else if (r.end === "error") note = t("cw_files_err_read");
  if (r.unreadable > 0) {
    const u = t("cw_files_size_unreadable_v2", { n: num(r.unreadable) });
    note = note ? `${note} ${u}` : u;
  }
  return note;
}

/** "Calculate folder size": a bounded walk (folderSize.ts) with its running count, Cancel, and the result. */
function FolderSize({ path }: { path: string }) {
  const [walk, setWalk] = useState<Walk>(null);
  const cancel = useRef<(() => void) | null>(null);
  useEffect(
    () => () => {
      // Closed while it walks: stop before the next page.
      cancel.current?.();
    },
    [],
  );
  // Calculate turns into Cancel and back: focus follows instead of falling out of the dialog.
  const phase = walk?.state ?? null;
  useEffect(() => {
    refocusIfLost(topModal(), phase === "running" ? ['[data-action="size-cancel"]'] : ['[data-action="size-start"]']);
  }, [phase]);
  const start = () => {
    const zero: SizeCount = { files: 0, folders: 0, bytes: 0, entries: 0, unreadable: 0 };
    const w = startFolderSize(path, (count) => setWalk({ state: "running", count }));
    if (!w) {
      setWalk({ state: "held" });
      return;
    }
    cancel.current = w.cancel;
    setWalk({ state: "running", count: zero });
    void w.done.then((result) => {
      cancel.current = null;
      setWalk({ state: "ended", result });
    });
  };
  if (walk?.state === "running") {
    return (
      <div className="cw-msg" role="status" data-size="running">
        {t("cw_files_size_running", { count: countText(walk.count) })}
        <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="size-cancel" onClick={() => cancel.current?.()}>
          <Square aria-hidden />
          <span>{t("cw_dialog_cancel")}</span>
        </button>
      </div>
    );
  }
  const note = walk?.state === "ended" ? walkNote(walk.result) : null;
  return (
    <>
      {walk?.state === "ended" && walk.result.end !== "error" ? (
        <dl className="cw-info cw-size-result" data-size={walk.result.end}>
          <div>
            <dt className="cw-dim">{t("cw_files_size_total")}</dt>
            <dd>
              {formatBytes(walk.result.bytes)} <span className="cw-dim">({t("cw_files_size_bytes", { n: num(walk.result.bytes) })})</span>
            </dd>
          </div>
          <div>
            <dt className="cw-dim">{t("cw_files_size_files")}</dt>
            <dd>{num(walk.result.files)}</dd>
          </div>
          <div>
            <dt className="cw-dim">{t("cw_files_size_folders")}</dt>
            <dd>{num(walk.result.folders)}</dd>
          </div>
        </dl>
      ) : null}
      {note ? <p className={walk?.state === "ended" && walk.result.end === "error" ? "cw-field-error" : "cw-dim"}>{note}</p> : null}
      {walk?.state === "held" ? <p className="cw-dim">{t("cw_files_size_busy")}</p> : null}
      <button type="button" className="cw-focusable cw-button cw-size-go" data-action="size-start" onClick={start}>
        <Calculator aria-hidden />
        <span>{t(walk?.state === "ended" ? "cw_files_size_again" : "cw_files_size_start")}</span>
      </button>
    </>
  );
}

/**
 * Info: type, size, modified time, permissions, and for a folder how many
 * entries it holds (the first page's count; "256+" when there are more), with
 * "Calculate folder size" walking the whole tree (bounded, cancellable).
 */
export function InfoDialog({
  dir,
  name,
  path,
  fsType,
  onClose,
}: {
  dir: string;
  name: string;
  path: string;
  /** The drive's file system (exFAT/FAT keep no permissions: the editor says so). */
  fsType?: string | null;
  onClose: () => void;
}) {
  const [info, setInfo] = useState<Info>(null);
  const [editing, setEditing] = useState(false);
  const [statTick, setStatTick] = useState(0);
  useEffect(() => {
    let live = true;
    (async () => {
      const stat = await call<Stat>("/api/fs/stat", { path });
      let items: string | null = null;
      if (stat.kind === "dir") {
        const r = await call<{ entries?: unknown[]; more?: boolean }>("/api/fs/list", { path, offset: 0, limit: LIST_PAGE });
        const n = Array.isArray(r?.entries) ? r.entries.length : 0;
        items = r?.more ? `${n}+` : String(n);
      }
      if (live) setInfo({ stat, items });
    })().catch(() => {
      if (live) setInfo({ error: true });
    });
    return () => {
      live = false;
    };
  }, [path, statTick]);

  const rows: [string, string][] = [[t("cw_files_info_where"), dir]];
  if (info && "stat" in info) {
    const s = info.stat;
    rows.push([t("cw_files_info_type"), kindText(s.kind)]);
    if (s.kind === "dir") rows.push([t("cw_files_info_items"), info.items ?? "—"]);
    else rows.push([t("cw_files_info_size"), formatBytes(s.size)]);
    if (s.mtime > 0) rows.push([t("cw_files_info_modified"), formatDateTime(s.mtime * 1000)]);
    rows.push([t("cw_files_info_mode"), `${modeText(s.mode)} ${modeLetters(s.mode)}`]);
  }
  const stat = info && "stat" in info ? info.stat : null;
  if (editing && stat) {
    return (
      <PermissionsDialog
        name={name}
        path={path}
        mode={stat.mode}
        dir={stat.kind === "dir"}
        fsType={fsType}
        onDone={() => {
          setEditing(false);
          setStatTick((n) => n + 1);
        }}
        onCancel={() => setEditing(false)}
      />
    );
  }

  return (
    <Modal label={name} onBack={onClose}>
      <h2 className="cw-file-name">{name}</h2>
      <dl className="cw-info">
        {rows.map(([k, v]) => (
          <div key={k}>
            <dt className="cw-dim">{k}</dt>
            <dd className={k === t("cw_files_info_where") ? "cw-mono" : undefined}>{v}</dd>
          </div>
        ))}
      </dl>
      {info && "stat" in info && info.stat.kind === "dir" ? <FolderSize path={path} /> : null}
      {info == null ? <p className="cw-dim">{t("cw_home_loading")}</p> : null}
      {info && "error" in info ? <p className="cw-field-error">{t("cw_files_err_read")}</p> : null}
      <div className="cw-dialog-buttons">
        {stat && stat.kind !== "link" ? (
          <button type="button" className="cw-focusable cw-button" data-action="permissions" onClick={() => setEditing(true)}>
            <ShieldCheck aria-hidden />
            <span>{t("cw_files_perm_open")}</span>
          </button>
        ) : null}
        <button type="button" className="cw-focusable cw-button" data-nav-default="" onClick={onClose}>
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
    </Modal>
  );
}
