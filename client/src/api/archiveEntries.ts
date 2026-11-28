// "Choose files" in an archive (#969): what a .zip, .7z, .rar or tar on this computer holds, as
// the rows the chooser ticks (engine /api/archive/entries). A plain fetch against the engine
// origin, so the desktop app and the web UI take the same path.

import { getEngineUrl } from "../state/engine";

/** What a package's header says (read for a package inside a ZIP only). */
export interface PackageBrief {
  title: string;
  title_id: string;
  content_id: string;
  /** `gd` a game, `gp` an update, `ac` DLC. */
  category: string;
  app_ver: string;
}

export interface ArchiveRow {
  /** The path inside the archive, as the transfer spells it. */
  path: string;
  kind: "package" | "folder" | "file";
  /** Unpacked bytes (a folder: everything in it). */
  size: number;
  /** Files it stands for. */
  files: number;
  package?: PackageBrief;
}

export interface ArchiveEntries {
  kind: "zip" | "7z" | "rar" | "tar";
  items: ArchiveRow[];
  file_count: number;
  total_bytes: number;
  /** False when the archive has too many rows to list (the rest are left out). */
  complete: boolean;
}

/** A big multi-part RAR is read header by header, across every part. */
const LIST_TIMEOUT_MS = 5 * 60_000;

/** The rows of the archive at `archivePath`. Throws the engine's words (a RAR password problem
 *  says `rar_password_required` / `rar_password_wrong`). */
export async function archiveEntries(
  archivePath: string,
  password?: string | null,
): Promise<ArchiveEntries> {
  const res = await fetch(`${getEngineUrl()}/api/archive/entries`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ archive_path: archivePath, password: password ?? null }),
    signal: AbortSignal.timeout(LIST_TIMEOUT_MS),
  });
  const body = (await res.json().catch(() => ({}))) as Partial<ArchiveEntries> & { error?: string };
  if (!res.ok) throw new Error(body.error || `request failed (${res.status})`);
  return {
    kind: body.kind ?? "zip",
    items: Array.isArray(body.items) ? body.items : [],
    file_count: body.file_count ?? 0,
    total_bytes: body.total_bytes ?? 0,
    complete: body.complete !== false,
  };
}
