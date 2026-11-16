/** Paths inside an image and the changes queued for an edited copy (#479). The engine applies
 *  the changes in order and checks each; these helpers only shape what the screen shows. */
import type { ImageEdit } from "../../api/fpkg";

/** `name` inside folder `dir` ("" at the root). */
export function joinPath(dir: string, name: string): string {
  return dir ? `${dir}/${name}` : name;
}

/** The last part of a path on this computer or inside an image. */
export function baseName(p: string): string {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

/** The folders from the root down to `dir`, for a breadcrumb: [{name, path}]. */
export function crumbs(dir: string): { name: string; path: string }[] {
  const out: { name: string; path: string }[] = [];
  let at = "";
  for (const part of dir.split("/").filter(Boolean)) {
    at = joinPath(at, part);
    out.push({ name: part, path: at });
  }
  return out;
}

/** The folder above `dir` ("" stays ""). */
export function parentDir(dir: string): string {
  const i = dir.lastIndexOf("/");
  return i < 0 ? "" : dir.slice(0, i);
}

/** The folder a saved file or folder landed in, to show it there. */
export function folderOf(dest: string): string {
  const i = Math.max(dest.lastIndexOf("/"), dest.lastIndexOf("\\"));
  return i <= 0 ? dest : dest.slice(0, i);
}

function within(path: string, target: string): boolean {
  return path === target || path.startsWith(`${target}/`);
}

/** What the queued changes do to the entry at `path`: deleted (it or a folder above it),
 *  renamed (it or a folder above it goes to another name), replaced (a file put on it, or
 *  something renamed onto its name), or nothing. The last change that touches it wins. */
export function pendingState(
  edits: ImageEdit[],
  path: string,
): "deleted" | "replaced" | "renamed" | null {
  let state: "deleted" | "replaced" | "renamed" | null = null;
  for (const e of edits) {
    if (e.op === "delete" && within(path, e.path)) state = "deleted";
    else if (e.op === "put" && e.path === path) state = "replaced";
    else if (e.op === "put" && within(path, e.path)) state = null;
    else if (e.op === "rename" && within(path, e.from)) state = "renamed";
    else if (e.op === "rename" && e.to === path) state = "replaced";
    else if (e.op === "mkdir" && e.path === path) state = null;
  }
  return state;
}

/** Where the image's own `path` is once the queued renames have run: a rename of it, or of a
 *  folder above it, takes it along. The path itself when no rename touches it. */
export function pathAfter(edits: ImageEdit[], path: string): string {
  let at = path;
  for (const e of edits) {
    if (e.op === "rename" && within(at, e.from)) at = e.to + at.slice(e.from.length);
  }
  return at;
}

/** Why `name` cannot be one file's or folder's name (the engine checks what the image's
 *  filesystem allows): "empty", or "separator" for a path or a `.`/`..`. `null` when usable. */
export function nameProblem(name: string): "empty" | "separator" | null {
  const n = name.trim();
  if (!n) return "empty";
  if (/[\\/]/.test(n) || n === "." || n === "..") return "separator";
  return null;
}

/** A key for one queued change in a list. */
export function editKey(e: ImageEdit, i: number): string {
  return e.op === "rename" ? `rename:${e.from}:${e.to}:${i}` : `${e.op}:${e.path}:${i}`;
}

/** Queues `edit`, dropping an earlier change it makes pointless: the same path put again, or
 *  a put at or under a path now deleted. Nothing queued before a rename is dropped: a rename
 *  changes what the paths before it meant, so an earlier put may be the very file that moved. */
export function queueEdit(edits: ImageEdit[], edit: ImageEdit): ImageEdit[] {
  if (edit.op !== "delete" && edit.op !== "put") return [...edits, edit];
  const keepUpTo = edits.reduce((last, e, i) => (e.op === "rename" ? i + 1 : last), 0);
  const kept = edits.filter((e, i) => {
    if (i < keepUpTo || e.op !== "put") return true;
    return edit.op === "delete" ? !within(e.path, edit.path) : e.path !== edit.path;
  });
  return [...kept, edit];
}
