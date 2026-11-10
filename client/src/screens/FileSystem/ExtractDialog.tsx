import { useEffect, useState } from "react";
import { FileArchive, HardDrive, Usb } from "lucide-react";

import { fsListDir, volumeForPath, type Volume } from "../../api/ps5";
import { Button, Checkbox, Input, Modal, SegmentedControl } from "../../components";
import { formatBytes } from "../../lib/format";
import { archiveFolderName, freeFolderName, validFolderName } from "../../lib/archiveName";
import { cleanConsolePath } from "../../lib/consolePath";
import { humanizeCaught } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { ConsoleFolderBrowser, DestButton } from "./ConsoleFolderBrowser";

/** What a folder that is already there gets: a new folder beside it, or the files merged in. */
export type ExtractMode = "new" | "merge" | "replace";

export interface ExtractChoice {
  /** The folder the new folder goes in: the archive's own by default, or another one, on any drive. */
  parent: string;
  /** The new folder's name, inside `parent`. */
  folder: string;
  merge: boolean;
  replace: boolean;
  password: string;
}

/** What to do once the archive is out: delete it (only when everything came out). */
export interface ExtractAfter {
  deleteArchive: boolean;
}

/** A drive with less room than the archive itself certainly cannot hold what is in it. */
export function extractRoomShort(archiveBytes: number, freeBytes: number | null | undefined): boolean {
  return typeof freeBytes === "number" && archiveBytes > 0 && freeBytes < archiveBytes;
}

/** `<parent>/<name>` on the console. */
export function extractPath(parent: string, name: string): string {
  return cleanConsolePath(`${parent}/${name}`);
}

/** The folder an extract writes into. The console writes each file beside its final name, so a
 *  folder on another drive never needs a move between drives. */
export function extractDest(choice: Pick<ExtractChoice, "parent" | "folder">): string {
  return extractPath(choice.parent, choice.folder);
}

/**
 * Which names are already in the chosen location: the listing shown (`exists`) when it is the
 * archive's folder and that listing is known, else the names read from that folder; null while
 * they are being read.
 */
export function namesAt(
  parent: string,
  dir: string,
  exists: ((n: string) => boolean) | undefined,
  names: string[] | null,
): ((n: string) => boolean) | null {
  if (exists && cleanConsolePath(parent) === cleanConsolePath(dir)) return exists;
  if (!names) return null;
  return (n) => names.includes(n);
}

/** The choice the dialog starts a job with: Keep both picks a free name, Merge keeps the one taken. */
export function extractChoice(
  parent: string,
  folder: string,
  mode: ExtractMode,
  exists: (n: string) => boolean,
  password: string,
): ExtractChoice {
  const taken = exists(folder);
  if (!taken || mode === "new")
    return { parent, folder: taken ? freeFolderName(folder, exists) : folder, merge: false, replace: false, password };
  return { parent, folder, merge: true, replace: mode === "replace", password };
}

/**
 * Extract an archive on the console: where it goes (next to the archive by
 * default, or any folder on any of the console's drives) and the new folder's
 * name (the archive's, by default), what to do when that folder is already there
 * (Keep both, Merge skipping files already there, Merge replacing them), and a
 * password for an archive that has one. `passwordAsk` reopens it to ask only
 * for the password after the console said it needs one (or that it was wrong).
 */
export function ExtractDialog({
  open,
  name,
  dir,
  exists,
  host = "",
  addr = "",
  volumes = [],
  listNames,
  passwordAsk = null,
  initial = null,
  archiveBytes = 0,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  /** The archive's file name. */
  name: string;
  /** The folder it is in. */
  dir: string;
  /** A name already in `dir` (the listing shown); without it, `dir` is read like any other folder. */
  exists?: (name: string) => boolean;
  /** The console (for its recent folders) and its address (to browse and read another folder). */
  host?: string;
  addr?: string;
  /** The console's writable drives: quick picks for where to extract, and their free space. */
  volumes?: Volume[];
  /** The names in another folder (default: listed on the console; tests pass their own). */
  listNames?: (dir: string) => Promise<string[]>;
  passwordAsk?: "required" | "wrong" | null;
  /** The choice of the run that needs a password: kept as it was. */
  initial?: ExtractChoice | null;
  /** The archive's size. */
  archiveBytes?: number;
  onCancel: () => void;
  onConfirm: (choice: ExtractChoice, after: ExtractAfter) => void;
}) {
  const tr = useTr();
  const [folder, setFolder] = useState(() => archiveFolderName(name));
  const [mode, setMode] = useState<ExtractMode>("new");
  const [password, setPassword] = useState("");
  const [deleteArchive, setDeleteArchive] = useState(false);
  const [parent, setParent] = useState(() => initial?.parent ?? dir);
  const [browsing, setBrowsing] = useState(false);
  const [names, setNames] = useState<string[] | null>(null);
  const [listError, setListError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setParent(initial?.parent ?? dir);
    setBrowsing(false);
    setFolder(initial?.folder ?? archiveFolderName(name));
    setMode(initial ? (initial.replace ? "replace" : initial.merge ? "merge" : "new") : "new");
    setPassword("");
    if (!initial) setDeleteArchive(false);
  }, [open, name, initial, dir]);

  const asking = passwordAsk != null;
  const elsewhere = cleanConsolePath(parent) !== cleanConsolePath(dir);
  const shown = exists != null;
  // Another folder: read what is in it, so a folder already there still offers Keep both / Merge.
  useEffect(() => {
    if (!open || asking || (!elsewhere && shown)) return;
    let live = true;
    setNames(null);
    setListError(null);
    const read = listNames ?? ((d: string) => fsListDir(addr, d).then((es) => es.map((e) => e.name)));
    read(parent).then(
      (ns) => {
        if (live) setNames(ns);
      },
      (e: unknown) => {
        if (!live) return;
        setNames(null);
        setListError(humanizeCaught(e));
      },
    );
    return () => {
      live = false;
    };
    // listNames is the default reader or a test's: the folder decides when to read.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, asking, elsewhere, shown, parent, addr]);

  const existsAt = namesAt(parent, dir, exists, names);
  const okName = validFolderName(folder.trim());
  const taken = okName && !asking && !!existsAt && existsAt(folder.trim());
  const blocked = !okName || (asking && password === "") || (!asking && (!existsAt || !!listError));
  const target = extractPath(parent, folder.trim());
  const targetVolume = volumeForPath(volumes, parent);
  const freeBytes = targetVolume ? targetVolume.free_bytes : null;

  const confirm = () => {
    if (blocked) return;
    if (asking && initial) {
      onConfirm({ ...initial, password }, { deleteArchive });
      return;
    }
    if (!existsAt) return;
    onConfirm(extractChoice(parent, folder.trim(), mode, existsAt, password), { deleteArchive });
  };

  const pick = (p: string) => {
    setParent(p);
    setListError(null);
  };

  return (
    <Modal
      open={open}
      onClose={onCancel}
      titleIcon={<FileArchive size={18} />}
      title={tr("fs_extract_title", { name }, `Extract "${name}"`)}
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            {tr("cancel", undefined, "Cancel")}
          </Button>
          <Button variant="primary" disabled={blocked} onClick={confirm} data-testid="fs-extract-go">
            {tr("fs_extract_confirm_label", undefined, "Extract")}
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-3 text-sm"
        onSubmit={(e) => {
          e.preventDefault();
          confirm();
        }}
      >
        {asking ? (
          <p role="alert" className="text-[var(--color-warn,inherit)]">
            {passwordAsk === "wrong"
              ? tr("fs_extract_password_wrong", undefined, "That password is wrong. Enter it again.")
              : tr("fs_extract_password_needed", undefined, "This archive is password-protected. Enter its password.")}
          </p>
        ) : (
          <>
            <p className="text-[var(--color-text-muted)]">
              {tr(
                "fs_extract_body_v2",
                undefined,
                "The console extracts it into a new folder and keeps the archive. Nothing goes through this computer.",
              )}
            </p>
            <div className="flex flex-col gap-2" data-testid="fs-extract-location">
              <div className="flex items-center gap-2">
                <span className="shrink-0 text-xs font-semibold text-[var(--color-muted)]">
                  {tr("fs_extract_location", undefined, "Extract to")}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-xs" title={parent} data-testid="fs-extract-parent">
                  {parent}
                </span>
                {freeBytes != null && (
                  <span className="shrink-0 text-xs text-[var(--color-muted)]">
                    {formatBytes(freeBytes)} {tr("fs_free", undefined, "free")}
                  </span>
                )}
                {addr && (
                  <Button size="sm" variant="ghost" onClick={() => setBrowsing((b) => !b)} data-testid="fs-extract-change">
                    {browsing
                      ? tr("fs_extract_location_done", undefined, "Done")
                      : tr("fs_extract_location_change", undefined, "Change…")}
                  </Button>
                )}
              </div>
              {elsewhere && (
                <button
                  type="button"
                  className="self-start text-xs text-[var(--color-accent)] hover:underline"
                  onClick={() => pick(dir)}
                  data-testid="fs-extract-beside"
                >
                  {tr("fs_extract_location_beside", undefined, "Back to the archive's folder")}
                </button>
              )}
              {volumes.length > 0 && (
                <div className="grid gap-1 sm:grid-cols-2" data-testid="fs-extract-drives">
                  {volumes.map((v) => {
                    const root = cleanConsolePath(v.path);
                    const Icon = root.startsWith("/mnt/usb") || root.startsWith("/mnt/ext") ? Usb : HardDrive;
                    return (
                      <DestButton key={root} path={root} selected={cleanConsolePath(parent) === root} onPick={pick}>
                        <Icon size={14} className="shrink-0" />
                        <span className="min-w-0 flex-1 truncate font-mono">{root}</span>
                        <span className="shrink-0 text-[var(--color-muted)]">
                          {formatBytes(v.free_bytes)} {tr("fs_free", undefined, "free")}
                        </span>
                      </DestButton>
                    );
                  })}
                </div>
              )}
              {browsing && addr && <ConsoleFolderBrowser open host={host} addr={addr} dest={parent} onDest={pick} />}
              {listError && (
                <p role="alert" className="text-xs text-[var(--color-danger,inherit)]" data-testid="fs-extract-location-error">
                  {tr("fs_move_dest_missing", undefined, "This folder can't be opened. Pick another.")}
                </p>
              )}
              {!existsAt && !listError && (
                <p className="text-xs text-[var(--color-text-muted)]">{tr("loading", undefined, "Loading…")}</p>
              )}
            </div>
            <Input
              label={tr("fs_extract_folder", undefined, "Folder")}
              value={folder}
              onChange={(e) => setFolder(e.target.value)}
              error={okName ? undefined : tr("fs_extract_folder_bad", undefined, "Use one folder name, without “/”.")}
              hint={target}
              autoComplete="off"
              spellCheck={false}
              data-testid="fs-extract-folder"
            />
            {taken && (
              <div className="flex flex-col gap-2" data-testid="fs-extract-exists">
                <p>
                  {elsewhere
                    ? tr("fs_extract_exists_there", { name: folder.trim() }, `A folder named "${folder.trim()}" is already there.`)
                    : tr("fs_extract_exists", { name: folder.trim() }, `A folder named "${folder.trim()}" is already here.`)}
                </p>
                <SegmentedControl
                  ariaLabel={tr("fs_extract_exists_label", undefined, "When the folder is already there")}
                  value={mode}
                  onChange={(v) => setMode(v as ExtractMode)}
                  segments={[
                    { value: "new", label: tr("fs_extract_mode_new", undefined, "Keep both") },
                    { value: "merge", label: tr("fs_extract_mode_merge", undefined, "Merge, skip existing files") },
                    { value: "replace", label: tr("fs_extract_mode_replace", undefined, "Merge, replace files") },
                  ]}
                />
                {mode === "new" && existsAt && (
                  <p className="text-[var(--color-text-muted)]">
                    {tr("fs_extract_new_into", { name: freeFolderName(folder.trim(), existsAt) }, `Extracts into "${freeFolderName(folder.trim(), existsAt)}".`)}
                  </p>
                )}
                {mode === "replace" && (
                  <p role="alert" className="text-[var(--color-danger,inherit)]">
                    {tr(
                      "fs_extract_replace_warn",
                      undefined,
                      "Files in that folder with the same name as one in the archive are overwritten. This can't be undone.",
                    )}
                  </p>
                )}
              </div>
            )}
          </>
        )}
        {!asking && extractRoomShort(archiveBytes, freeBytes) && (
          <p role="alert" className="text-[var(--color-warn,inherit)]" data-testid="fs-extract-room">
            {tr(
              "fs_extract_room_short_v2",
              { free: formatBytes(freeBytes ?? 0), size: formatBytes(archiveBytes) },
              `This drive has ${formatBytes(freeBytes ?? 0)} free, less than the archive itself (${formatBytes(archiveBytes)}): what is in it will not fit. Extract to another drive, or free some space first.`,
            )}
          </p>
        )}
        {!asking && (
          <Checkbox
            checked={deleteArchive}
            onChange={setDeleteArchive}
            label={tr("fs_extract_delete_after", undefined, "Delete the archive after extracting")}
            hint={tr(
              "fs_extract_delete_after_hint",
              undefined,
              "Only when every file came out. A split archive's other parts stay.",
            )}
          />
        )}
        <Input
          type="password"
          label={
            asking
              ? tr("fs_extract_password_label", undefined, "Password")
              : tr("fs_extract_password", undefined, "Password (only if it has one)")
          }
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          autoComplete="off"
          autoFocus={asking}
          data-testid="fs-extract-password"
        />
      </form>
    </Modal>
  );
}
