import type { SearchHit } from "../../api/ps5";

/** Where a search result opens in File System: a folder opens itself, a file opens the
 *  folder that holds it (File System browses folders; `?path=` is its deep link). */
export function fileSystemLinkFor(hit: Pick<SearchHit, "path" | "kind">): string {
  const path = hit.path.replace(/\/+$/, "") || "/";
  let dir = path;
  if (hit.kind !== "dir") {
    const cut = path.lastIndexOf("/");
    dir = cut > 0 ? path.slice(0, cut) : "/";
  }
  return `/files?path=${encodeURIComponent(dir)}`;
}
