import { useCallback, useEffect, useRef, useState } from "react";
import {
  ChevronRight,
  Download,
  File as FileIcon,
  FilePlus,
  Folder,
  FolderOpen,
  FolderPen,
  FolderPlus,
  FolderSearch,
  Pencil,
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
import { useConfirm, usePrompt } from "../../components/ConfirmDialog";
import {
  imageBrowse,
  jobCancel,
  jobStatus,
  type ImageBrowseListing,
  type JobStageSnapshot,
} from "../../api/ps5";
import { fpkg, type ImageEdit, type InPlaceCheck } from "../../api/fpkg";
import { BROWSE_IMAGE_EXTENSIONS } from "./imageTypes";
import {
  baseName,
  crumbs,
  editKey,
  folderOf,
  joinPath,
  nameProblem,
  parentDir,
  pathAfter,
  pendingState,
  queueEdit,
} from "./browseEdits";

/** A save, an edited copy or an in-place edit the engine is running. */
interface RunningJob {
  id: string;
  kind: "save" | "edit" | "inplace";
  what: string;
  done: number;
  total: number;
  stage?: JobStageSnapshot;
}

interface Finished {
  kind: "save" | "edit" | "inplace";
  dest: string;
  /** Done, but with something to say (an in-place edit whose journal removal may not have
   *  reached the disk: the changes are in the image and checked). */
  warning?: string;
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
  const { prompt, dialog: promptDialog } = usePrompt();
  const { confirm, dialog: confirmDialog } = useConfirm();
  // Whether the pending changes can go into the image itself (asked of the engine as they
  // change); null while unknown or not offered.
  const [inPlace, setInPlace] = useState<InPlaceCheck | null>(null);
  const alive = useRef(true);
  const applying = useRef(false);
  useEffect(
    () => () => {
      alive.current = false;
    },
    [],
  );

  // The image shown now: a listing that comes back for another one (a reload after a job on the
  // image picked before) is dropped, or its rows would be edited under the new image's name.
  const current = useRef<string | null>(null);
  const load = useCallback(async (path: string, dir: string) => {
    setLoading(true);
    setError(null);
    try {
      const l = await imageBrowse(path, dir);
      if (alive.current && current.current === path) setListing(l);
    } catch (e) {
      if (alive.current && current.current === path) setError(String(e));
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
      current.current = picked;
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
      // The image the job is about: if another is opened meanwhile, its end (changes cleared,
      // the saved banner, an error) is not written onto that one.
      const about = current.current;
      const here = () => alive.current && current.current === about;
      setFinished(null);
      setJob({ id, kind, what, done: 0, total: 0 });
      try {
        for (;;) {
          const snap = await jobStatus(id);
          if (!alive.current) return;
          if (snap.status === "done") {
            if (!here()) return;
            const warning = snap.commit_ack?.warning;
            setFinished({ kind, dest: snap.dest ?? "", ...(typeof warning === "string" && warning ? { warning } : {}) });
            if (kind === "edit" || kind === "inplace") setEdits([]);
            return;
          }
          if (snap.status === "failed") {
            if (here()) setError(snap.error ?? tr("imgbrowse_job_failed", undefined, "It did not finish."));
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
        if (here()) setError(String(e));
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

  // Ask whether the changes fit in place, a moment after they stop changing.
  const offersInPlace = !!listing?.in_place;
  const jobRunning = !!job;
  useEffect(() => {
    setInPlace(null);
    if (!image || !offersInPlace || edits.length === 0 || jobRunning) return;
    let stale = false;
    const t = setTimeout(() => {
      fpkg
        .checkEditInPlace(image, edits)
        .then((c) => {
          if (!stale && alive.current) setInPlace(c);
        })
        .catch(() => {
          if (!stale && alive.current) setInPlace(null);
        });
    }, 400);
    return () => {
      stale = true;
      clearTimeout(t);
    };
  }, [image, edits, offersInPlace, jobRunning]);

  const applyInPlace = useCallback(async () => {
    if (!image || edits.length === 0 || applying.current) return;
    const name = baseName(image);
    // One at a time: a second click while the first is asking or starting does nothing.
    applying.current = true;
    const ok = await confirm({
      title: tr("imgbrowse_inplace_confirm_title", { name }, `Change ${name} itself?`),
      message: tr(
        "imgbrowse_inplace_confirm",
        { name },
        `The changes are written into ${name} itself; no copy is made. An undo journal is kept beside it while they go in, so if this is interrupted (the app closes, the computer loses power) the image is put back as it was the next time it is opened. Every changed file is read back, and if anything does not check out the image is restored.`,
      ),
      confirmLabel: tr("imgbrowse_inplace_apply", undefined, "Apply in place"),
    });
    if (!ok) {
      applying.current = false;
      return;
    }
    setError(null);
    try {
      const { job_id } = await fpkg.editImageInPlace(image, edits);
      await follow(job_id, "inplace", name);
      if (alive.current && current.current === image) await load(image, listing?.dir ?? "");
    } catch (e) {
      // Refused (it didn't fit, the image is in use): the edited copy is still there to use.
      if (current.current === image) setError(String(e));
    } finally {
      applying.current = false;
    }
  }, [confirm, edits, follow, image, listing?.dir, load, tr]);

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
      // In a folder a queued rename moves, the new file goes where the folder will be.
      setEdits((cur) =>
        queueEdit(cur, {
          op: "put",
          path: at ?? joinPath(pathAfter(cur, listing.dir), baseName(from)),
          from,
        }),
      );
    },
    [listing, tr],
  );

  /** Asks for one file or folder name; `null` when cancelled or unusable (said as an error).
   *  What the image's filesystem allows in a name is the engine's to check, when the copy is
   *  written. */
  const askName = useCallback(
    async (title: string, message: string, defaultValue: string): Promise<string | null> => {
      const typed = await prompt({ title, message, defaultValue });
      if (typed === null) return null;
      if (nameProblem(typed)) {
        setError(
          tr(
            "imgbrowse_name_bad",
            undefined,
            "A name cannot be empty, and cannot contain / or \\. Nothing was changed.",
          ),
        );
        return null;
      }
      return typed.trim();
    },
    [prompt, tr],
  );

  /** Queues a rename of the listed entry `name` (in the folder shown) to a typed name. */
  const rename = useCallback(
    async (path: string, name: string) => {
      const to = await askName(
        tr("imgbrowse_rename_title", { name }, `Rename ${name}`),
        tr(
          "imgbrowse_rename_body",
          undefined,
          "The new name, in the same folder. Nothing else in that folder may have it. The image you opened is not changed: the edited copy has the new name.",
        ),
        name,
      );
      if (!to || to === name) return;
      setError(null);
      setEdits((cur) => {
        // The entry as the changes queued so far leave it: it may sit in a renamed folder.
        const from = pathAfter(cur, path);
        return queueEdit(cur, { op: "rename", from, to: joinPath(parentDir(from), to) });
      });
    },
    [askName, tr],
  );

  /** Queues a new empty folder in the folder shown. */
  const newFolder = useCallback(async () => {
    if (!listing) return;
    const name = await askName(
      tr("imgbrowse_mkdir_title", undefined, "New folder"),
      tr(
        "imgbrowse_mkdir_body",
        undefined,
        "The name of a new, empty folder here. It is kept in the edited copy even when nothing is put in it.",
      ),
      "",
    );
    if (!name) return;
    setError(null);
    setEdits((cur) => queueEdit(cur, { op: "mkdir", path: joinPath(pathAfter(cur, listing.dir), name) }));
  }, [askName, listing, tr]);

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
          "imgbrowse_explain_v2",
          undefined,
          "Browse any game image or package ps5upload reads (.exfat, .ffpkg, .ffpfs, .ffpfsc, or a .pkg made with the debug key) on any computer, without mounting it. Save a file or folder to the computer, or change files and write an edited copy. The image you opened changes only if you choose Apply in place.",
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
              : finished.kind === "inplace"
                ? tr("imgbrowse_inplace_done", undefined, "Changes applied to the image and checked")
                : tr("imgbrowse_save_done", undefined, "Saved and checked")
          }
          detail={finished.warning ? `${finished.dest} — ${finished.warning}` : finished.dest}
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
                : job.kind === "inplace"
                  ? tr("imgbrowse_applying_inplace", { name: job.what }, `Applying the changes to ${job.what}`)
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
                <Button
                  size="sm"
                  variant="secondary"
                  leftIcon={<FolderPen size={13} />}
                  disabled={busy}
                  onClick={() => void newFolder()}
                >
                  {tr("imgbrowse_new_folder", undefined, "New folder")}
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
                  {state === "renamed" &&
                    // Its own name changes, or only a folder above it does.
                    (baseName(pathAfter(edits, path)) !== e.name ? (
                      <Badge tone="warn">
                        {tr(
                          "imgbrowse_renamed",
                          { name: baseName(pathAfter(edits, path)) },
                          `will be renamed to ${baseName(pathAfter(edits, path))}`,
                        )}
                      </Badge>
                    ) : (
                      <Badge tone="warn">{tr("imgbrowse_moved", undefined, "will be moved")}</Badge>
                    ))}
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
                    {editable && !e.is_dir && state !== "renamed" && (
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
                    {/* One rename per entry; a deleted or already renamed one is undone in the
                        list of changes first. */}
                    {editable && state !== "deleted" && state !== "renamed" && (
                      <Button
                        size="sm"
                        variant="ghost"
                        aria-label={tr("imgbrowse_rename_item", { name: e.name }, `Rename ${e.name}`)}
                        title={tr("imgbrowse_rename_item", { name: e.name }, `Rename ${e.name}`)}
                        disabled={busy}
                        onClick={() => void rename(path, e.name)}
                      >
                        <Pencil size={13} />
                      </Button>
                    )}
                    {editable && state !== "deleted" && state !== "renamed" && (
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
              <li key={editKey(e, i)} className="flex items-center gap-2">
                <span className="min-w-0 flex-1 truncate">
                  {e.op === "delete"
                    ? tr("imgbrowse_change_delete", { path: e.path }, `Delete ${e.path}`)
                    : e.op === "rename"
                      ? tr(
                          "imgbrowse_change_rename",
                          { from: e.from, to: e.to },
                          `Rename ${e.from} to ${e.to}`,
                        )
                      : e.op === "mkdir"
                        ? tr("imgbrowse_change_mkdir", { path: e.path }, `New folder ${e.path}`)
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
            {inPlace?.in_place && (
              <Button
                variant="secondary"
                size="sm"
                leftIcon={<Pencil size={13} />}
                disabled={busy}
                onClick={() => void applyInPlace()}
                title={tr(
                  "imgbrowse_inplace_hint",
                  undefined,
                  "Write the changes into this image instead of making a copy",
                )}
              >
                {tr("imgbrowse_inplace_apply", undefined, "Apply in place")}
              </Button>
            )}
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => setEdits([])}>
              {tr("imgbrowse_discard", undefined, "Discard changes")}
            </Button>
          </div>
          {inPlace && !inPlace.in_place && inPlace.reason && (
            <p className="mt-2 text-xs text-[var(--color-muted)]">
              {tr(
                "imgbrowse_inplace_no",
                { reason: inPlace.reason },
                `These changes can't be applied to the image itself (${inPlace.reason}); write an edited copy instead.`,
              )}
            </p>
          )}
        </div>
      )}
      {promptDialog}
      {confirmDialog}
    </Card>
  );
}
