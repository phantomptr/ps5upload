// The one way to add games: files, folders, or every game in one folder. Upload & Install and
// the Workshop both start from "which games?", so they share these buttons, their labels and
// how they pick: several files or folders at once (the system dialog's multi-select, ticked
// rows in the in-app browser), one from a saved server through the ▾, and "Add games from a
// folder…" for a folder that holds games. What a screen does with what was picked is its own.

import { useState, type ReactNode } from "react";
import { File as FileIcon, ListPlus } from "lucide-react";

import { Button } from "./Button";
import { BrowseButton } from "./BrowseButton";
import { pickPath, pickPaths, type PickPathOptions } from "../lib/pickPath";
import { useTr } from "../state/lang";

/** Game images, packages and archives: what a game file can be. */
export const GAME_FILE_FILTERS: NonNullable<PickPathOptions["filters"]> = [
  {
    name: "Game image, package or archive",
    // A dialog filter matches the last extension only: "zst", "gz", "xz" and "bz2" cover the
    // compressed tars (.tar.zst …) and a lone .zst.
    extensions: [
      "exfat", "ffpkg", "ffpfs", "ffpfsc", "pkg", "zip", "7z", "rar",
      "tar", "zst", "tzst", "gz", "tgz", "xz", "txz", "bz2", "tbz2", "tbz",
    ],
  },
];

export interface AddSourcesProps {
  /** Files picked on the engine's computer (one or several). */
  onFiles: (paths: string[]) => void;
  /** Folders picked on the engine's computer (one or several). */
  onFolders: (paths: string[]) => void;
  /** One file or folder picked on a saved server. */
  onRemote: (path: string, isDir: boolean) => void;
  /** The folder picked for "Add games from a folder…": the screen finds the games in it. */
  onScanFolder: (folder: string) => void;
  /** The file dialog's types; all files when absent. */
  fileFilters?: PickPathOptions["filters"];
  disabled?: boolean;
  /** A pick that failed (not cancelled: the dialog or the in-app browser could not open).
   *  When absent the message shows under the buttons. */
  onError?: (message: string) => void;
  /** Extra ways in that only one screen has (the Workshop's "On the PS5…"). */
  children?: ReactNode;
  className?: string;
}

/** Runs a pick; a failure (not a cancel, which resolves empty) is reported, not swallowed. */
export async function pickOrReport<R>(
  pick: () => Promise<R>,
  empty: R,
  report: (e: unknown) => void,
): Promise<R> {
  try {
    return await pick();
  } catch (e) {
    report(e);
    return empty;
  }
}

export function AddSources(p: AddSourcesProps) {
  const tr = useTr();
  const [error, setError] = useState<string | null>(null);
  const report = () => {
    const msg = tr(
      "add_sources_no_picker",
      undefined,
      "This build cannot browse this machine — type a path the engine can see.",
    );
    if (p.onError) p.onError(msg);
    else setError(msg);
  };
  const pickMany = async (mode: "file" | "folder") => {
    setError(null);
    const picked = await pickOrReport(() => pickPaths({
      mode,
      title:
        mode === "folder"
          ? tr("add_sources_pick_folders", undefined, "Choose one or more folders")
          : tr("add_sources_pick_files", undefined, "Choose one or more files"),
      filters: mode === "file" ? p.fileFilters : undefined,
    }), [] as string[], report);
    if (!picked.length) return;
    if (mode === "folder") p.onFolders(picked);
    else p.onFiles(picked);
  };
  const scan = async () => {
    setError(null);
    const folder = await pickOrReport<string | null>(
      () =>
        pickPath({
          mode: "folder",
          title: tr("batch_scan_pick", undefined, "Choose the folder that holds your games"),
        }),
      null,
      report,
    );
    if (folder) p.onScanFolder(folder);
  };
  return (
    <div className={`flex flex-wrap items-center justify-center gap-2 ${p.className ?? ""}`}>
      <BrowseButton
        mode="file"
        remote
        icon={<FileIcon size={14} />}
        label={tr("upload_choose_file", undefined, "Choose file")}
        filters={p.fileFilters}
        disabled={p.disabled}
        onMainClick={() => void pickMany("file")}
        onPick={(path) => p.onRemote(path, false)}
      />
      <BrowseButton
        mode="folder"
        remote
        label={tr("upload_choose_folder", undefined, "Choose folder")}
        disabled={p.disabled}
        onMainClick={() => void pickMany("folder")}
        onPick={(path) => p.onRemote(path, true)}
      />
      <Button variant="ghost" size="sm" leftIcon={<ListPlus size={14} />} disabled={p.disabled} onClick={() => void scan()}>
        {tr("batch_scan", undefined, "Add games from a folder…")}
      </Button>
      {p.children}
      {error && (
        <p role="alert" data-testid="add-sources-error" className="w-full text-center text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}
    </div>
  );
}
