import { useEffect, useState } from "react";
import { FileArchive, HardDrive, Usb } from "lucide-react";

import { fsExtractList, fsListDir, volumeForPath, type ExtractListing, type Volume } from "../../api/ps5";
import { Button, Checkbox, Input, Modal, SegmentedControl } from "../../components";
import { formatBytes } from "../../lib/format";
import { archiveFolderName, freeFolderName, validFolderName } from "../../lib/archiveName";
import { cleanConsolePath, keepBothName } from "../../lib/consolePath";
import { humanizeCaught } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { ConsoleFolderBrowser, DestButton } from "./ConsoleFolderBrowser";

/** What a folder that is already there gets: a new folder beside it, or the files merged in. */
export type ExtractMode = "new" | "merge" | "replace";

export interface ExtractChoice {
  /** The folder the new folder goes in: the archive's own by default, or another one, on any drive. */
  parent: string;
  /** The new folder's name, inside `parent` ("" for Extract here). */
  folder: string;
  merge: boolean;
  replace: boolean;
  password: string;
  /** Extract here (#891): straight into `parent`, no new folder. */
  here?: boolean;
  /** Extract here: top-level entries put under another name (Keep both) or left out (Skip). */
  top?: { from: string; to: string }[];
}

/** Where the archive goes: a new folder (the default), or straight into the folder. */
export type ExtractWhere = "new" | "here";

/** What Extract here does with a top-level entry already in the folder. */
export type HereConflict = "replace" | "keep" | "skip";

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
export function extractDest(choice: Pick<ExtractChoice, "parent" | "folder" | "here">): string {
  return choice.here ? cleanConsolePath(choice.parent) : extractPath(choice.parent, choice.folder);
}

/** Whether the console takes `dir` as a folder to extract straight into: never a drive's own top
 *  folder (/data, /mnt/usb0), as with any extract (the helper's fsj_path_is_root). */
export function extractHereAllowed(dir: string): boolean {
  const parts = cleanConsolePath(dir).split("/").filter(Boolean);
  return parts[0] === "mnt" ? parts.length >= 3 : parts.length >= 2;
}

/** The archive's top-level entries already in the folder. */
export function hereConflicts(top: string[], exists: (n: string) => boolean): string[] {
  return top.filter((n) => exists(n));
}

/**
 * The job for Extract here: into `parent`, writing into it (merge). With conflicts, `policy`
 * decides: Replace overwrites files of the same name (a folder merges into the one there), Keep
 * both puts each incoming entry under a free name ("a (2).txt"), Skip leaves those entries out.
 * Without a policy (no conflicts) nothing there is ever overwritten.
 */
export function hereChoice(
  parent: string,
  listing: Pick<ExtractListing, "top" | "dirs">,
  conflicts: string[],
  policy: HereConflict | null,
  exists: (n: string) => boolean,
  password: string,
): ExtractChoice {
  const base: ExtractChoice = { parent, folder: "", here: true, merge: true, replace: false, password };
  if (conflicts.length === 0 || !policy) return base;
  if (policy === "replace") return { ...base, replace: true };
  if (policy === "skip") return { ...base, top: conflicts.map((from) => ({ from, to: "" })) };
  const used = new Set<string>(listing.top);
  const top = conflicts.map((from) => {
    const dir = listing.dirs.includes(from);
    let to = from;
    for (let n = 2; n < 10_000 && (exists(to) || used.has(to)); n++) to = keepBothName(from, n, dir);
    used.add(to);
    return { from, to };
  });
  return { ...base, top };
}

/** Why a listing failed, as the dialog says it. */
export function listErrorKind(message: string): "password" | "wrong_password" | "old_helper" | "other" {
  if (message.includes("fs_extract_password_required")) return "password";
  if (message.includes("fs_extract_wrong_password")) return "wrong_password";
  if (message.includes("fs_extract_list_unsupported") || message.includes("fs_extract_destination_exists"))
    return "old_helper";
  return "other";
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
  listTop,
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
  /** The archive's top-level names, for Extract here (default: read on the console; tests pass
   *  their own). */
  listTop?: (parent: string, password: string) => Promise<ExtractListing>;
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
  const [where, setWhere] = useState<ExtractWhere>(() => (initial?.here ? "here" : "new"));
  /** Extract here: reading the archive's names, the conflicts found, or why it could not. */
  const [checking, setChecking] = useState(false);
  const [hereAsk, setHereAsk] = useState<{ listing: ExtractListing; conflicts: string[] } | null>(null);
  const [hereError, setHereError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setParent(initial?.parent ?? dir);
    setBrowsing(false);
    setWhere(initial?.here ? "here" : "new");
    setChecking(false);
    setHereAsk(null);
    setHereError(null);
    setFolder(initial?.folder || archiveFolderName(name));
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
  const here = where === "here";
  const hereOk = extractHereAllowed(parent);
  const okName = here || validFolderName(folder.trim());
  const taken = !here && okName && !asking && !!existsAt && existsAt(folder.trim());
  const blocked =
    !okName ||
    checking ||
    (asking && password === "") ||
    (!asking && (!existsAt || !!listError)) ||
    (!asking && here && !hereOk);
  const target = here ? cleanConsolePath(parent) : extractPath(parent, folder.trim());
  const targetVolume = volumeForPath(volumes, parent);
  const freeBytes = targetVolume ? targetVolume.free_bytes : null;

  const confirm = () => {
    if (blocked) return;
    if (asking && initial) {
      onConfirm({ ...initial, password }, { deleteArchive });
      return;
    }
    if (!existsAt) return;
    if (here) {
      void checkHere(existsAt);
      return;
    }
    onConfirm(extractChoice(parent, folder.trim(), mode, existsAt, password), { deleteArchive });
  };

  // Extract here: the archive's top-level names against the folder's, asked about once, before
  // anything is written. Nothing there is overwritten unless Replace is picked.
  const checkHere = async (existsNow: (n: string) => boolean) => {
    setChecking(true);
    setHereError(null);
    setHereAsk(null);
    const read =
      listTop ?? ((p: string, pw: string) => fsExtractList(addr, extractPath(dir, name), p, { password: pw || undefined }));
    try {
      const listing = await read(parent, password);
      const conflicts = hereConflicts(listing.top, existsNow);
      if (conflicts.length === 0) {
        onConfirm(hereChoice(parent, listing, [], null, existsNow, password), { deleteArchive });
        return;
      }
      setHereAsk({ listing, conflicts });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      const kind = listErrorKind(msg);
      setHereError(
        kind === "password"
          ? tr("fs_extract_here_password", undefined, "This archive is password-protected. Enter its password, then Extract.")
          : kind === "wrong_password"
            ? tr("fs_extract_password_wrong", undefined, "That password is wrong. Enter it again.")
            : kind === "old_helper"
              ? tr("fs_extract_here_old_helper", undefined, "The helper on this PS5 is too old for Extract here. Update the helper, or extract into a new folder.")
              : tr("fs_extract_here_list_failed", { error: humanizeCaught(e) }, "Couldn't read what is in the archive: {error}"),
      );
    } finally {
      setChecking(false);
    }
  };

  const settleHere = (policy: HereConflict) => {
    if (!hereAsk || !existsAt) return;
    onConfirm(hereChoice(parent, hereAsk.listing, hereAsk.conflicts, policy, existsAt, password), { deleteArchive });
  };

  const pick = (p: string) => {
    setParent(p);
    setListError(null);
    setHereAsk(null);
    setHereError(null);
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
              {here
                ? tr(
                    "fs_extract_here_body",
                    undefined,
                    "The console extracts it straight into this folder and keeps the archive. Nothing goes through this computer.",
                  )
                : tr(
                    "fs_extract_body_v2",
                    undefined,
                    "The console extracts it into a new folder and keeps the archive. Nothing goes through this computer.",
                  )}
            </p>
            <SegmentedControl
              ariaLabel={tr("fs_extract_where_label", undefined, "Where the files go")}
              value={where}
              onChange={(v) => {
                setWhere(v as ExtractWhere);
                setHereAsk(null);
                setHereError(null);
              }}
              segments={[
                { value: "new", label: tr("fs_extract_where_new", undefined, "Into a new folder") },
                { value: "here", label: tr("fs_extract_where_here", undefined, "Extract here") },
              ]}
            />
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
            {!here && (
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
            )}
            {here && !hereOk && (
              <p role="alert" className="text-xs text-[var(--color-danger,inherit)]" data-testid="fs-extract-here-root">
                {tr(
                  "fs_extract_here_root",
                  undefined,
                  "Files can't be extracted straight into a drive's top folder. Pick a folder on it, or extract into a new folder.",
                )}
              </p>
            )}
            {here && checking && (
              <p className="text-xs text-[var(--color-text-muted)]" data-testid="fs-extract-here-checking">
                {tr("fs_extract_here_checking", undefined, "Checking what is in the archive…")}
              </p>
            )}
            {here && hereError && (
              <p role="alert" className="text-xs text-[var(--color-danger,inherit)]" data-testid="fs-extract-here-error">
                {hereError}
              </p>
            )}
            {here && hereAsk && (
              <div className="flex flex-col gap-2" data-testid="fs-extract-here-conflicts">
                <p>
                  {tr(
                    "fs_extract_here_conflicts",
                    { n: hereAsk.conflicts.length, names: hereAsk.conflicts.slice(0, 3).join(", ") },
                    "{n} of the archive's items are already in this folder ({names}). What should happen to them?",
                  )}
                </p>
                {hereAsk.listing.more && (
                  <p className="text-xs text-[var(--color-text-muted)]">
                    {tr(
                      "fs_extract_here_more",
                      undefined,
                      "The archive has more items than could be checked. Files already here that weren't checked are kept as they are.",
                    )}
                  </p>
                )}
                <div className="flex flex-wrap gap-2">
                  {/* Keep both first and focused: Enter never overwrites unasked. */}
                  <Button size="sm" variant="primary" autoFocus onClick={() => settleHere("keep")} data-testid="fs-extract-here-keep">
                    {tr("fs_extract_here_keep", undefined, "Keep both")}
                  </Button>
                  <Button size="sm" variant="ghost" onClick={() => settleHere("skip")} data-testid="fs-extract-here-skip">
                    {tr("fs_extract_here_skip", undefined, "Skip existing")}
                  </Button>
                  <Button size="sm" variant="danger" onClick={() => settleHere("replace")} data-testid="fs-extract-here-replace">
                    {tr("fs_extract_here_replace", undefined, "Replace")}
                  </Button>
                </div>
                <p className="text-xs text-[var(--color-text-muted)]">
                  {tr(
                    "fs_extract_here_conflicts_hint",
                    undefined,
                    "Keep both gives the incoming items a new name. Skip existing leaves those items out. Replace overwrites files with the same name, and folders are merged; that can't be undone.",
                  )}
                </p>
              </div>
            )}
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
