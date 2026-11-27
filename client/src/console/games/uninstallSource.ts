/**
 * What Uninstall may also delete, on the console page: the folder or disk image a game we
 * registered plays from. The same rules as the engine's (engine/.../uninstall_source.rs): only
 * a title registered from a folder or one of our image mounts (/mnt/ps5upload/*); never a drive
 * root, a top folder of /data, the console's own folders, a source another registered game
 * shares, or a folder holding another game. A package install, a ShadowMount+ title, or a source
 * already gone: nothing to ask, the plain uninstall.
 */
import { ApiError, call } from "../api";
import { walkFolder, type WalkDeps } from "../files/folderSize";
import { watchOp } from "../files/opJob";
import { parseRegistered } from "./model";
import type { CallFn } from "./model";

export interface SourcePlan {
  kind: "folder" | "image";
  path: string;
  /** An image's mount, unmounted before the image goes. */
  mountPoint: string | null;
  /** What deleting frees; null when it could not be read. */
  bytes: number | null;
  /** The folder walk stopped early: at least `bytes`. */
  partial: boolean;
}

type Resolved = { kind: "folder" | "image"; path: string; mountPoint: string | null } | { reason: string };

const IMAGE_EXTS = [".ffpkg", ".exfat", ".ffpfs", ".ffpfsc", ".img"];
const RESERVED_TOP = ["user", "ps5upload", "system", "system_ex", "preinst"];

const clean = (p: string) => p.replace(/\/+$/, "") || "/";
const within = (a: string, b: string) => a === b || a.startsWith(`${b}/`);

/** A path Uninstall may delete as a game's own (see the module comment). */
export function deletable(p: string): boolean {
  if (!p.startsWith("/")) return false;
  const comps = p.split("/").slice(1);
  if (comps.some((c) => c === "" || c === "." || c === "..")) return false;
  let rest: string[];
  let data = false;
  if (comps[0] === "data") {
    rest = comps.slice(1);
    data = true;
  } else if (comps[0] === "mnt" && /^(ext|usb)\d{1,2}$/.test(comps[1] ?? "")) {
    rest = comps.slice(2);
  } else return false;
  if (!rest.length) return false;
  if (RESERVED_TOP.includes(rest[0].toLowerCase())) return false;
  return !data || rest.length >= 2;
}

/** What `titleId` plays from, from the registered list and the volumes, or why nothing. */
export function resolveSource(
  titleId: string,
  apps: { titleId: string; source: string }[],
  volumes: { path: string; source_image?: string }[],
): Resolved {
  const id = titleId.toUpperCase();
  const app = apps.find((a) => a.titleId.toUpperCase() === id);
  if (!app || !app.source.trim()) return { reason: "not_registered" };
  const src = clean(app.source);
  if (within(src, "/mnt/shadowmnt")) return { reason: "shadowmount" };
  const others = apps.filter((a) => a.titleId.toUpperCase() !== id && a.source.trim()).map((a) => clean(a.source));
  if (within(src, "/mnt/ps5upload")) {
    let best: { path: string; image: string } | null = null;
    for (const v of volumes) {
      const p = clean(v.path);
      if (p !== "/mnt/ps5upload" && within(src, p) && (!best || p.length > best.path.length))
        best = { path: p, image: (v.source_image ?? "").trim() };
    }
    if (!best) return { reason: "image_not_mounted" };
    if (!best.image) return { reason: "image_unknown" };
    const image = clean(best.image);
    if (!IMAGE_EXTS.some((e) => image.toLowerCase().endsWith(e)) || !deletable(image)) return { reason: "unsafe_path" };
    if (others.some((o) => within(o, best.path))) return { reason: "shared_image" };
    return { kind: "image", path: image, mountPoint: best.path };
  }
  if (!deletable(src)) return { reason: "unsafe_path" };
  if (others.some((o) => within(o, src) || within(src, o))) return { reason: "shared_folder" };
  return { kind: "folder", path: src, mountPoint: null };
}

interface Stat {
  kind?: string;
  size?: number;
  missing?: true;
}

/** What Uninstall of `titleId` can offer to delete, read now; null when nothing (or unreadable). */
export async function planSource(titleId: string, callFn: CallFn = call, walk?: WalkDeps): Promise<SourcePlan | null> {
  try {
    const reg = parseRegistered(await callFn<unknown>("/api/apps/registered"));
    if (!reg.whole) return null;
    const vols = await callFn<{ volumes?: { path: string; source_image?: string }[] }>("/api/volumes").catch(() => null);
    const r = resolveSource(titleId, [...reg.apps.values()], vols?.volumes ?? []);
    if ("reason" in r) return null;
    const stat = (path: string) => callFn<Stat>("/api/fs/stat", { path, missing_ok: true });
    const st = await stat(r.path);
    if (st.missing || st.kind !== (r.kind === "image" ? "file" : "dir")) return null;
    if (r.kind === "image") return { ...r, bytes: typeof st.size === "number" ? st.size : null, partial: false };
    if ((await stat(`${r.path}/sce_sys/param.json`)).missing) return null;
    let nested = false;
    const size = await walkFolder(
      r.path,
      {
        cancelled: () => nested,
        onDir: (parent, name) => {
          if (parent !== r.path && name.toLowerCase() === "sce_sys") nested = true;
        },
      },
      walk,
    );
    if (nested || size.end === "error") return null;
    return { ...r, bytes: size.bytes, partial: size.end !== "done" };
  } catch {
    return null;
  }
}

/** Delete what `plan` names, after the uninstall: unmount an image first, then delete. */
export async function deleteSource(
  plan: SourcePlan,
  callFn: CallFn = call,
  wait: (job: string) => Promise<void> = waitJob,
): Promise<void> {
  if (plan.mountPoint) await callFn("/api/volumes/unmount", { path: plan.mountPoint });
  const r = await callFn<{ job?: unknown }>("/api/fs/delete", { path: plan.path });
  if (!r || typeof r.job !== "string") throw new ApiError(500, "bad_reply");
  await wait(r.job);
}

async function waitJob(job: string): Promise<void> {
  const end = await watchOp(job);
  if (end.end === "done" || end.end === "gone") return;
  throw new ApiError(500, (end.status?.message ?? "").split(":")[0] || "io_error");
}
