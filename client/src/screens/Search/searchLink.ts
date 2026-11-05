import type { SearchHit } from "../../api/ps5";

/** Where a search result opens in Files: a folder opens itself, a file opens the folder
 *  that holds it with the file selected (Files browses folders). */
export function fileSystemTargetFor(hit: Pick<SearchHit, "path" | "kind">): { dir: string; select?: string } {
  const path = hit.path.replace(/\/+$/, "") || "/";
  if (hit.kind === "dir") return { dir: path };
  const cut = path.lastIndexOf("/");
  return { dir: cut > 0 ? path.slice(0, cut) : "/", select: path.slice(cut + 1) || undefined };
}
