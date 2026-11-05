// Small helpers for paths on a console, used by the app and by Minion alike. Console paths are
// always `/`-separated, so these never apply to a path on this computer.

/** Without repeated or trailing slashes. */
export function cleanConsolePath(p: string): string {
  const c = p.replace(/\/{2,}/g, "/");
  return c.length > 1 ? c.replace(/\/$/, "") : c;
}

/** The folder a path is in; the root is its own parent. */
export function consoleParentDir(p: string): string {
  if (p === "/" || p === "") return "/";
  const i = p.lastIndexOf("/");
  return i <= 0 ? "/" : p.slice(0, i);
}

/** The n-th "Keep both" name: `a.txt` → `a (2).txt`. `a (1).txt` counts on as `a (2).txt`, and a
 *  folder's dots are part of its name. The Files upload and Minion's paste must agree on this. */
export function keepBothName(name: string, n: number, dir: boolean): string {
  const dot = dir ? -1 : name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  const ext = dot > 0 ? name.slice(dot) : "";
  const m = /^(.+) \((\d+)\)$/.exec(stem);
  return `${m ? m[1] : stem} (${n})${ext}`;
}
