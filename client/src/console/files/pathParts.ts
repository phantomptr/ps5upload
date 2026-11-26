/** The path bar's pieces and the "Go to path" check (pure, apart from checkFolder's one call). */
import { ApiError, call, isOffline } from "../api";
import { errorText } from "../home/Home";
import { isDrive, volumeLabel, type Volume } from "../home/Home";
import { t } from "../strings";
import { cleanPath } from "./prefs";
import { volumeOf } from "./move";

export interface Segment {
  label: string;
  path: string;
}

export type BarItem = { kind: "seg"; seg: Segment; last: boolean } | { kind: "more"; hidden: Segment[] };

/** Longer paths than this many segments show the first, "…" and the last two. */
export const MAX_SEGMENTS = 4;

/**
 * `path` as segments to jump to: the drive (by its name) or "/" first, then
 * each folder. A path outside every volume starts at "/".
 */
export function pathSegments(path: string, volumes: Volume[]): Segment[] {
  const p = cleanPath(path);
  const vol = volumeOf(p, volumes.filter((v) => v.path !== "/"));
  const out: Segment[] = [];
  let root: string;
  if (vol) {
    root = vol.path.replace(/\/+$/, "");
    out.push({ label: isDrive(vol) ? volumeLabel(vol) : root, path: root });
  } else {
    root = "";
    out.push({ label: "/", path: "/" });
  }
  const rest = p.slice(root.length).split("/").filter(Boolean);
  let at = root;
  for (const name of rest) {
    at = `${at}/${name}`;
    out.push({ label: name, path: at });
  }
  return out;
}

/** What the bar shows: every segment, or (collapsed) the first, "…", and the last two. */
export function barItems(segs: Segment[], expanded: boolean): BarItem[] {
  const seg = (s: Segment, i: number): BarItem => ({ kind: "seg", seg: s, last: i === segs.length - 1 });
  if (expanded || segs.length <= MAX_SEGMENTS) return segs.map(seg);
  const tail = segs.length - 2;
  return [seg(segs[0], 0), { kind: "more", hidden: segs.slice(1, tail) }, seg(segs[tail], tail), seg(segs[tail + 1], tail + 1)];
}

/** A typed path as the page uses it, or null when it cannot be one (not absolute, "." / ".."). */
export function typedPath(raw: string): string | null {
  const v = raw.trim();
  if (!v.startsWith("/") || v.indexOf("\0") >= 0) return null;
  const p = cleanPath(v);
  if (p.split("/").some((part) => part === "." || part === "..")) return null;
  return p;
}

/**
 * "Go to path…": null when `raw` names a folder on the console, else why not
 * (shown under the text box).
 */
export async function checkFolder(raw: string): Promise<string | null> {
  const p = typedPath(raw);
  if (!p) return t("cw_files_goto_bad");
  try {
    const st = await call<{ kind?: unknown }>("/api/fs/stat", { path: p });
    if (st?.kind === "dir") return null;
    return t("cw_files_goto_not_folder");
  } catch (e) {
    if (e instanceof ApiError) {
      if (e.status === 404 || /not_found|no_such|enoent|stat_failed/i.test(e.code)) return t("cw_files_goto_missing");
      if (e.status === 403) return t("cw_files_err_denied");
      // the console out of reach or restarting: said as such, not as a code
      if (isOffline(e)) return errorText(e);
      return t("cw_files_goto_failed", { code: e.code });
    }
    return t("cw_files_goto_failed", { code: String(e) });
  }
}
