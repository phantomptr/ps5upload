import { useCallback, useEffect, useRef, useState } from "react";
import {
  ChevronRight,
  Download,
  File as FileIcon,
  FilePlus,
  Folder,
  FolderOpen,
  FolderPlus,
  FolderSearch,
  Replace,
  Save,
  Trash2,
  Undo2,
  X,
} from "lucide-react";
import {
  Badge,
  Button,
  Callout,
  Card,
  ErrorCard,
  ProgressBar,
  Spinner,
  SuccessCard,
} from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { formatBytes } from "../../lib/format";
import { pickPath } from "../../lib/pickPath";
import { openLocalPath } from "../../lib/openLocalPath";
import { useTr } from "../../state/lang";
import {
  imageBrowse,
  jobCancel,
  jobStatus,
  type ImageBrowseListing,
  type JobStageSnapshot,
} from "../../api/ps5";
import { fpkg, type ImageEdit } from "../../api/fpkg";
import { BROWSE_IMAGE_EXTENSIONS } from "./imageTypes";
import {
  baseName,
  crumbs,
  folderOf,
  joinPath,
  parentDir,
  pendingState,
  queueEdit,
} from "./browseEdits";

/** A save or an edited copy the engine is running. */
interface RunningJob {
  id: string;
  kind: "save" | "edit";
  what: string;
  done: number;
  total: number;
  stage?: JobStageSnapshot;
}

interface Finished {
  kind: "save" | "edit";
  dest: string;
}

const FS_LABEL: Record<ImageBrowseListing["filesystem"], string> = {
  exfat: "exFAT",
  ufs2: "UFS2",
  pfs: "PFS",
  pkg: "PKG",
};

/** Look inside any game image or debug package on this computer (#479): list folders with
 *  sizes, save a file or folder to the computer, and (for images) write an edited copy. */
export function ImageBrowser({
  source,
  onChoose,
  onClose,
}: {
  /** The image to show, chosen elsewhere (Workshop's one game source). */
  source?: string | null;
  /** When given, picking another image (or the edited copy) hands it here instead of opening it
   *  in place, so the screen's one source follows it. */
  onChoose?: (path: string) => void;
  /** Shown as a Close button: back to what the screen was doing before looking inside. */
  onClose?: () => void;
} = {}) {
  const tr = useTr();
  const [image, setImage] = useState<string | null>(null);
  const [listing, setListing] = useState<ImageBrowseListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [edits, setEdits] = useState<ImageEdit[]>([]);
  const [job, setJob] = useState<RunningJob | null>(null);
  const [finished, setFinished] = useState<Finished | null>(null);
  const alive = useRef(true);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );

  const load = useCallback(async (path: string, dir: string) => {
    setLoading(true);
    setError(null);
    try {
      const l = await imageBrowse(path, dir);
      if (alive.current) setListing(l);
    } catch (e) {
      if (alive.current) setError(String(e));
    } finally {
      if (alive.current) setLoading(false);
    }
  }, []);

  const openImage = useCallback(
    async (path?: string, fromSource = false) => {
      const picked =
        path ??
        (await pickPath({
          mode: "file",
          title: tr("imgbrowse_pick", undefined, "Choose an image or package to look inside"),
          filters: [
            {
              name: tr("imgbrowse_filter", undefined, "Game images and packages"),
              extensions: [...BROWSE_IMAGE_EXTENSIONS],
            },
          ],
        }));
      if (!picked) return;
      if (onChoose && !fromSource) {
        onChoose(picked);
        return;
      }
      setImage(picked);
      setListing(null);
      setEdits([]);
      setFinished(null);
      await load(picked, "");
    },
    [load, tr, onChoose],
  );

  // A source chosen on the screen opens here, and a new one replaces it.
  useEffect(() => {
    if (source) void openImage(source, true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [source]);

  /** Follows a job to its end, updating the progress shown. */
  const follow = useCallback(
    async (id: string, kind: RunningJob["kind"], what: string) => {
      setFinished(null);
      setJob({ id, kind, what, done: 0, total: 0 });
      try {
        for (;;) {
          const snap = await jobStatus(id);
          if (!alive.current) return;
          if (snap.status === "done") {
            setFinished({ kind, dest: snap.dest ?? "" });
            if (kind === "edit") setEdits([]);
            return;
          }
          if (snap.status === "failed") {
            setError(snap.error ?? tr("imgbrowse_job_failed", undefined, "It did not finish."));
            return;
          }
          setJob({
            id,
            kind,
            what,
            done: snap.bytes_sent ?? 0,
            total: snap.total_bytes ?? 0,
            stage: snap.stage,
          });
          await new Promise((r) => setTimeout(r, 500));
        }
      } catch (e) {
        if (alive.current) setError(String(e));
      } finally {
        if (alive.current) setJob(null);
      }
    },
    [tr],
  );

  const save = useCallback(
    async (entry: string, label: string) => {
      if (!image) return;
      const dest = await pickPath({
        mode: "folder",
        title: tr("imgbrowse_save_where", { name: label }, `Save ${label} into…`),
      });
      if (!dest) return;
      setError(null);
      try {
        const { job_id } = await fpkg.buildFolder(image, dest, undefined, false, entry || undefined);
        await follow(job_id, "save", label);
      } catch (e) {
        setError(String(e));
      }
    },
    [follow, image, tr],
  );

  const writeCopy = useCallback(async () => {
    if (!image || edits.length === 0) return;
    setError(null);
    try {
      const { job_id } = await fpkg.editImage(image, edits);
      await follow(job_id, "edit", baseName(image));
    } catch (e) {
      setError(String(e));
    }
  }, [edits, follow, image]);

  const putFrom = useCallback(
    async (mode: "file" | "folder", at?: string) => {
      const from = await pickPath({
        mode,
        title:
          mode === "folder"
            ? tr("imgbrowse_pick_folder", undefined, "Choose a folder to add")
            : tr("imgbrowse_pick_file", undefined, "Choose a file"),
      });
      if (!from || !listing) return;
      const path = at ?? joinPath(listing.dir, baseName(from));
      setEdits((cur) => queueEdit(cur, { op: "put", path, from }));
    },
    [listing, tr],
  );

  const busy = loading || !!job;
  const dir = listing?.dir ?? "";
  const editable = !!listing?.editable;

  const showFolder = useCallback(
    async (dest: string, isFolder: boolean) => {
      const opened = await openLocalPath(isFolder ? dest : folderOf(dest));
      if (!opened) {
        setError(
          tr(
            "imgbrowse_show_failed",
            { path: dest },
            `This computer would not open it. It is at ${dest}.`,
          ),
        );
      }
    },
    [tr],
  );

  return (
    <Card
      className="mb-5"
      title={tr("imgbrowse_title", undefined, "Look inside an image")}
      icon={FolderSearch}
      right={
        <div className="flex flex-wrap gap-2">
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<FolderOpen size={13} />}
            onClick={() => void openImage()}
            disabled={busy}
          >
            {image
              ? tr("imgbrowse_open_other", undefined, "Open another")
              : tr("imgbrowse_open", undefined, "Choose an image")}
          </Button>
          {onClose && (
            <Button variant="ghost" size="sm" leftIcon={<X size={13} />} onClick={onClose} disabled={busy}>
              {tr("close", undefined, "Close")}
            </Button>
          )}
        </div>
      }
    >
      <p className="mb-3 text-sm text-[var(--color-muted)]">
        {tr(
          "imgbrowse_explain",
          undefined,
          "Browse any game image or package ps5upload reads (.exfat, .ffpkg, .ffpfs, .ffpfsc, or a .pkg made with the debug key) on any computer, without mounting it. Save a file or folder to the computer, or change files and write an edited copy: the image you opened is never changed.",
        )}
      </p>

      {error && (
        <ErrorCard
          title={tr("localimage_failed", undefined, "Something went wrong")}
          detail={humanizePs5Error(error) || error}
          onDismiss={() => setError(null)}
        />
      )}

      {finished && (
        <SuccessCard
          title={
            finished.kind === "edit"
              ? tr("imgbrowse_copy_done", undefined, "Edited copy written and checked")
              : tr("imgbrowse_save_done", undefined, "Saved and checked")
          }
          detail={finished.dest}
          onDismiss={() => setFinished(null)}
          action={
            <div className="flex gap-2">
              {finished.kind === "edit" && (
                <Button size="sm" variant="secondary" onClick={() => void openImage(finished.dest)}>
                  {tr("imgbrowse_open_copy", undefined, "Look inside the copy")}
                </Button>
              )}
              <Button
                size="sm"
                variant="secondary"
                onClick={() => void showFolder(finished.dest, false)}
              >
                {tr("imgbrowse_show", undefined, "Show in folder")}
              </Button>
            </div>
          }
        />
      )}

      {job && (
        <div className="mb-3 rounded-md border border-[var(--color-border)] p-3">
          <div className="mb-2 flex items-center justify-between gap-3 text-sm">
            <span className="truncate">
              {job.kind === "edit"
                ? tr("imgbrowse_writing_copy", { name: job.what }, `Writing an edited copy of ${job.what}`)
                : tr("imgbrowse_saving", { name: job.what }, `Saving ${job.what}`)}
              {job.stage?.id === "verify"
                ? ` · ${tr("imgbrowse_checking", undefined, "checking")}`
                : ""}
            </span>
            <Button
              size="sm"
              variant="ghost"
              leftIcon={<X size={13} />}
              onClick={() => void jobCancel(job.id)}
            >
              {tr("cancel", undefined, "Cancel")}
            </Button>
          </div>
          <ProgressBar
            label={job.what}
            value={
              job.stage && job.stage.total > 0
                ? job.stage.done / job.stage.total
                : job.total > 0
                  ? job.done / job.total
                  : null
            }
          />
        </div>
      )}

      {image && (
        <div className="mb-2 flex flex-wrap items-center gap-2">
          <span className="truncate text-sm font-medium">{baseName(image)}</span>
          {listing && (
            <>
              <Badge tone="accent">{FS_LABEL[listing.filesystem]}</Badge>
              {listing.compressed && (
                <Badge tone="neutral">
                  {tr("imgbrowse_compressed", undefined, "compressed")}
                </Badge>
              )}
              <span className="text-xs text-[var(--color-muted)]">
                {tr(
                  "imgbrowse_totals",
                  { files: listing.total_files, size: formatBytes(listing.total_bytes) },
                  `${listing.total_files} files, ${formatBytes(listing.total_bytes)}`,
                )}
              </span>
            </>
          )}
        </div>
      )}

      {listing && !listing.editable && (
        <Callout
          tone="info"
          className="mb-3"
          title={tr("imgbrowse_pkg_readonly_title", undefined, "Packages are read-only here")}
        >
          {tr(
            "imgbrowse_pkg_readonly_v2",
            undefined,
            "A package can be looked inside and saved from, not edited. To change one, save it as a folder, change the folder, and build a new package from that folder here.",
          )}
        </Callout>
      )}

      {listing && (
        <>
          <nav className="mb-2 flex flex-wrap items-center gap-1 text-sm" aria-label={tr("imgbrowse_path", undefined, "Folder")}>
            <button
              type="button"
              className="rounded px-1 hover:bg-[var(--color-surface-2)]"
              onClick={() => image && void load(image, "")}
              disabled={busy}
            >
              {baseName(image ?? "")}
            </button>
            {crumbs(dir).map((c) => (
              <span key={c.path} className="flex items-center gap-1">
                <ChevronRight size={12} className="text-[var(--color-muted)]" />
                <button
                  type="button"
                  className="rounded px-1 hover:bg-[var(--color-surface-2)]"
                  onClick={() => image && void load(image, c.path)}
                  disabled={busy}
                >
                  {c.name}
                </button>
              </span>
            ))}
            {loading && <Spinner size={12} />}
          </nav>

          <div className="mb-3 flex flex-wrap gap-2">
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<Download size={13} />}
              disabled={busy}
              onClick={() => void save(dir, dir ? baseName(dir) : baseName(image ?? ""))}
            >
              {tr("imgbrowse_save_folder", undefined, "Save this folder")}
            </Button>
            {editable && (
              <>
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<FilePlus size={13} />}
                  disabled={busy}
                  onClick={() => void putFrom("file")}
                >
                  {tr("imgbrowse_add_file", undefined, "Add a file here")}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<FolderPlus size={13} />}
                  disabled={busy}
                  onClick={() => void putFrom("folder")}
                >
                  {tr("imgbrowse_add_folder", undefined, "Add a folder here")}
                </Button>
              </>
            )}
          </div>

          <div className="overflow-hidden rounded-md border border-[var(--color-border)]">
            {dir && (
              <button
                type="button"
                className="flex w-full items-center gap-2 border-b border-[var(--color-border)] px-3 py-2 text-start text-sm hover:bg-[var(--color-surface-2)]"
                onClick={() => image && void load(image, parentDir(dir))}
                disabled={busy}
              >
                <Folder size={14} className="text-[var(--color-muted)]" />
                ..
              </button>
            )}
            {listing.entries.length === 0 && (
              <div className="px-3 py-4 text-sm text-[var(--color-muted)]">
                {tr("imgbrowse_empty", undefined, "This folder is empty.")}
              </div>
            )}
            {listing.entries.map((e) => {
              const path = joinPath(dir, e.name);
              const state = pendingState(edits, path);
              return (
                <div
                  key={path}
                  className="flex flex-wrap items-center gap-2 border-b border-[var(--color-border)] px-3 py-1.5 text-sm last:border-b-0"
                >
                  <button
                    type="button"
                    className={`flex min-w-0 flex-1 items-center gap-2 text-start ${
                      state === "deleted" ? "line-through opacity-60" : ""
                    } ${e.is_dir ? "hover:underline" : "cursor-default"}`}
                    onClick={() => e.is_dir && image && void load(image, path)}
                    disabled={busy || !e.is_dir}
                  >
                    {e.is_dir ? (
                      <Folder size={14} className="shrink-0 text-[var(--color-accent)]" />
                    ) : (
                      <FileIcon size={14} className="shrink-0 text-[var(--color-muted)]" />
                    )}
                    <span className="truncate">{e.name}</span>
                  </button>
                  {state === "replaced" && (
                    <Badge tone="warn">{tr("imgbrowse_replaced", undefined, "will be replaced")}</Badge>
                  )}
                  {state === "deleted" && (
                    <Badge tone="bad">{tr("imgbrowse_deleted", undefined, "will be deleted")}</Badge>
                  )}
                  <span className="w-24 shrink-0 text-end font-mono text-xs text-[var(--color-muted)]">
                    {formatBytes(e.size)}
                  </span>
                  <div className="flex shrink-0 gap-1">
                    <Button
                      size="sm"
                      variant="ghost"
                      aria-label={tr("imgbrowse_save_item", { name: e.name }, `Save ${e.name} to this computer`)}
                      title={tr("imgbrowse_save_item", { name: e.name }, `Save ${e.name} to this computer`)}
                      disabled={busy}
                      onClick={() => void save(path, e.name)}
                    >
                      <Download size={13} />
                    </Button>
                    {editable && !e.is_dir && (
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={tr("imgbrowse_replace_item", { name: e.name }, `Replace ${e.name}`)}
                        title={tr("imgbrowse_replace_item", { name: e.name }, `Replace ${e.name}`)}
                        disabled={busy}
                        onClick={() => void putFrom("file", path)}
                      >
                        <Replace size={13} />
                      </Button>
                    )}
                    {editable && state !== "deleted" && (
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={tr("imgbrowse_delete_item", { name: e.name }, `Delete ${e.name}`)}
                        title={tr("imgbrowse_delete_item", { name: e.name }, `Delete ${e.name}`)}
                        disabled={busy}
                        onClick={() => setEdits((cur) => queueEdit(cur, { op: "delete", path }))}
                      >
                        <Trash2 size={13} />
                      </Button>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}

      {edits.length > 0 && (
        <div className="mt-4 rounded-md border border-[var(--color-border)] p-3">
          <div className="mb-2 text-sm font-medium">
            {tr("imgbrowse_changes", { count: edits.length }, `Changes for the edited copy (${edits.length})`)}
          </div>
          <ul className="mb-3 space-y-1 text-sm">
            {edits.map((e, i) => (
              <li key={`${e.op}:${e.path}:${i}`} className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate">
                  {e.op === "delete"
                    ? tr("imgbrowse_change_delete", { path: e.path }, `Delete ${e.path}`)
                    : tr(
                        "imgbrowse_change_put",
                        { path: e.path, from: baseName(e.from) },
                        `Put ${baseName(e.from)} at ${e.path}`,
                      )}
                </span>
                <Button
                  size="sm"
                  variant="ghost"
                  aria-label={tr("imgbrowse_undo", undefined, "Undo")}
                  title={tr("imgbrowse_undo", undefined, "Undo")}
                  disabled={busy}
                  onClick={() => setEdits((cur) => cur.filter((_, j) => j !== i))}
                >
                  <Undo2 size={13} />
                </Button>
              </li>
            ))}
          </ul>
          <p className="mb-3 text-xs text-[var(--color-muted)]">
            {tr(
              "imgbrowse_copy_explain",
              undefined,
              "The copy is a whole new image of the same kind, written next to the original with -edited in its name, then read back and checked. It takes about as long as converting the game. The original is not changed.",
            )}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button
              variant="primary"
              size="sm"
              leftIcon={<Save size={13} />}
              disabled={busy}
              onClick={() => void writeCopy()}
            >
              {tr("imgbrowse_write_copy", undefined, "Write edited copy")}
            </Button>
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => setEdits([])}>
              {tr("imgbrowse_discard", undefined, "Discard changes")}
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}
