// Upload with several sources: the review list. Each row is inspected in the background (four
// at a time) with the same inspectors a single pick uses; a row's late result after it was
// removed or re-inspected is dropped.

import { create } from "zustand";

import {
  inspectFolder,
  rarInspect,
  sevenzInspectStream,
  zipInspectStream,
} from "../api/ps5";
import { remoteApi } from "../api/remote";
import { archiveDefault, type ArchiveChoice } from "../lib/archiveDefault";
import { invoke } from "../lib/invokeLogged";
import { isInstallPackagePath } from "../lib/pkgDropDedupe";
import { isRemotePath } from "../lib/remotePath";
import { archiveFormat, isImagePath, type PickedSource } from "./upload";
import { platformFromTitleId, titleIdFromContentId } from "./pkgLibrary";

export type BatchStatus = "inspecting" | "ready" | "needs-password" | "error";

export interface BatchRow {
  id: string;
  path: string;
  isDir: boolean;
  /** Bytes, when a listing said (a folder scan); null otherwise. */
  size: number | null;
  /** Left unchecked rows stay listed but are not added. */
  include: boolean;
  status: BatchStatus;
  source: PickedSource | null;
  error: string | null;
  /** An encrypted .rar's password; memory only. */
  password: string | null;
  /** An archive: unpack it or send it as it is (#645). Null until the user chooses, when its
   *  listing could not tell; such a row holds the list back. Unused for other kinds. */
  archiveChoice: ArchiveChoice | null;
}

export interface BatchEntry {
  path: string;
  isDir: boolean;
  size?: number | null;
  include?: boolean;
}

type Inspector = (path: string, isDir: boolean, password: string | null) => Promise<PickedSource>;

/** What a single pick would learn about `path`, as a `PickedSource`. */
export async function inspectSource(
  path: string,
  isDir: boolean,
  password: string | null,
): Promise<PickedSource> {
  const base = { path, meta: null, wrappedHint: null, zipInfo: null };
  if (isDir) {
    const inspection = isRemotePath(path)
      ? await remoteApi.inspectFolder(path)
      : await inspectFolder(path);
    return {
      ...base,
      kind: inspection.result.meta_source !== "none" ? "game-folder" : "folder",
      meta: inspection.result,
      wrappedHint: inspection.wrapped_hint,
    };
  }
  if (isInstallPackagePath(path)) {
    const meta = (await invoke("pkg_metadata_split", { path })) as {
      head?: { content_id?: string; title?: string; category?: string; platform?: string };
      parts?: unknown[];
      total_size?: number;
    };
    if ((meta.parts?.length ?? 1) > 1) throw new Error("Split .pkg sets aren't supported — pick the single lead .pkg.");
    const cid = meta.head?.content_id ?? "";
    return {
      ...base,
      kind: "pkg",
      pkgInfo: {
        contentId: cid,
        title: meta.head?.title ?? null,
        category: meta.head?.category ?? null,
        totalBytes: meta.total_size ?? 0,
        platform: meta.head?.platform || platformFromTitleId(titleIdFromContentId(cid)),
      },
    };
  }
  const fmt = archiveFormat(path);
  if (fmt) {
    // A server archive is copied here when its turn comes in the queue; nothing to read now.
    if (isRemotePath(path)) return { ...base, kind: "archive" };
    const zipInfo =
      fmt === "rar"
        ? await rarInspect(path, password)
        : await (fmt === "7z" ? sevenzInspectStream : zipInspectStream)(path, () => {});
    return { ...base, kind: "archive", zipInfo };
  }
  return { ...base, kind: isImagePath(path) ? "image" : "file" };
}

let inspector: Inspector = inspectSource;
/** Tests swap the inspector. */
export function setBatchInspector(f: Inspector) {
  inspector = f;
}

const LIMIT = 4;
let seq = 0;
/** Bumped per inspection of a row: only the newest result for a row lands. */
const generation = new Map<string, number>();
const waiting: string[] = [];
let active = 0;

interface BatchState {
  rows: BatchRow[];
  add: (entries: BatchEntry[]) => void;
  remove: (id: string) => void;
  setInclude: (id: string, include: boolean) => void;
  setPassword: (id: string, password: string) => void;
  setArchiveChoice: (id: string, choice: ArchiveChoice) => void;
  clear: () => void;
}

export const useUploadBatch = create<BatchState>((set, get) => {
  const patch = (id: string, p: Partial<BatchRow>) =>
    set({ rows: get().rows.map((r) => (r.id === id ? { ...r, ...p } : r)) });

  const pump = () => {
    while (active < LIMIT && waiting.length) {
      const id = waiting.shift()!;
      const row = get().rows.find((r) => r.id === id);
      if (!row) continue;
      const gen = (generation.get(id) ?? 0) + 1;
      generation.set(id, gen);
      active++;
      void inspector(row.path, row.isDir, row.password)
        .then((source) => {
          if (generation.get(id) !== gen) return;
          // A choice the user made stays; otherwise the archive's listing picks one.
          const chosen = get().rows.find((r) => r.id === id)?.archiveChoice ?? null;
          patch(id, {
            status: "ready",
            source,
            error: null,
            archiveChoice:
              source.kind === "archive" ? chosen ?? archiveDefault(source.zipInfo) : null,
          });
        })
        .catch((e: unknown) => {
          if (generation.get(id) !== gen) return;
          const msg = e instanceof Error ? e.message : String(e);
          if (/rar_password_(required|wrong)/.test(msg)) {
            patch(id, {
              status: "needs-password",
              error: /wrong/.test(msg) ? "The password is wrong." : null,
            });
          } else {
            patch(id, { status: "error", error: msg, include: false });
          }
        })
        .finally(() => {
          active--;
          pump();
        });
    }
  };

  const inspect = (id: string) => {
    waiting.push(id);
    pump();
  };

  return {
    rows: [],
    add: (entries) => {
      const have = new Set(get().rows.map((r) => r.path));
      const fresh: BatchRow[] = [];
      for (const e of entries) {
        if (have.has(e.path)) continue;
        have.add(e.path);
        fresh.push({
          id: `b${++seq}`,
          path: e.path,
          isDir: e.isDir,
          size: e.size ?? null,
          include: e.include ?? true,
          status: "inspecting",
          source: null,
          error: null,
          password: null,
          archiveChoice: null,
        });
      }
      if (!fresh.length) return;
      set({ rows: [...get().rows, ...fresh] });
      for (const r of fresh) inspect(r.id);
    },
    remove: (id) => {
      generation.set(id, (generation.get(id) ?? 0) + 1);
      set({ rows: get().rows.filter((r) => r.id !== id) });
    },
    setInclude: (id, include) => patch(id, { include }),
    setArchiveChoice: (id, archiveChoice) => patch(id, { archiveChoice }),
    setPassword: (id, password) => {
      patch(id, { password, status: "inspecting", error: null });
      inspect(id);
    },
    clear: () => {
      for (const r of get().rows) generation.set(r.id, (generation.get(r.id) ?? 0) + 1);
      waiting.length = 0;
      set({ rows: [] });
    },
  };
});
