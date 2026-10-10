// The Convert queue: games to build on this computer, one at a time (a build uses this
// computer's CPU and disk, not a console). When a build that should install finishes, its
// package is handed to the console queue and the next build starts — game 2 converts while game
// 1 installs. The running build is shown by the Convert screen's own stage view.

import { hostOf } from "../lib/addr";
import { create } from "zustand";

import type { FpkgCompression, ImageFormat } from "../api/fpkg";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

export type ConvertThen = "keep" | "stream" | "upload";
export type ConvertStatus = "pending" | "running" | "installing" | "handed" | "done" | "failed";

/** What a queued game becomes: an installable package, or a game image (compressed into a
 *  .ffpfsc when `compress`). Only a game folder can become an image. */
export type ConvertBuild = { kind: "pkg" } | { kind: "image"; format: ImageFormat; compress: boolean };

/** The file a build makes, by its extension: what the queue row and the add button say. */
export function buildExtension(build: ConvertBuild | undefined): string {
  if (!build || build.kind === "pkg") return ".pkg";
  return build.compress ? ".ffpfsc" : `.${build.format}`;
}

export interface ConvertItem {
  id: string;
  /** A game folder, image or archive, here or on a server or console. */
  source: string;
  outputDir?: string;
  compression: FpkgCompression;
  /** Absent: a package (items queued before images could be). */
  build?: ConvertBuild;
  /** An image sent to the PS5 once built (then = "upload"): where it goes. */
  imageDest?: { volume: string | null; subpath: string };
  firmware?: string;
  /** The one PlayGo language the package declares; every language when absent. */
  language?: string;
  /** What to do with the package: keep it, or install it (streamed / uploaded first). An image
   *  is kept, or ("upload") put in the Upload queue for the PS5. */
  then: ConvertThen;
  /** The console to install on. */
  host: string | null;
  /** Delete the package once its install is verified (only ever a package Convert built). */
  deleteAfterInstall?: boolean;
  status: ConvertStatus;
  packagePath?: string;
  error?: string;
}

export type NewConvertItem = Omit<ConvertItem, "id" | "status" | "packagePath" | "error">;

/** How the queue builds and installs; the app wires the real ones (see convertQueueRunner). */
export interface ConvertRunner {
  /** `installed`: the build also installed it (a console dump's swap runs inside the build). */
  build(item: ConvertItem): Promise<{ ok: boolean; packagePath?: string; message?: string; installed?: boolean }>;
  handOff(item: ConvertItem, packagePath: string): Promise<{ ok: boolean; message?: string }>;
  cancel(): Promise<void>;
}

let runner: ConvertRunner | null = null;
export function setConvertRunner(r: ConvertRunner) {
  runner = r;
}

const KEY = "ps5upload.convert_queue.v1";

/** After a restart: a build that was running is built again (its engine died with the app, or
 *  the rebuild clears its partial output); an install already handed over belongs to the
 *  console queue now. */
export function hydrateConvertQueue(items: ConvertItem[]): ConvertItem[] {
  return items.map((i) =>
    i.status === "running"
      ? { ...i, status: "pending" as const }
      : i.status === "installing"
        ? { ...i, status: "handed" as const }
        : i,
  );
}

function load(): ConvertItem[] {
  try {
    const raw = safeGetItem(KEY);
    return raw ? hydrateConvertQueue(JSON.parse(raw) as ConvertItem[]) : [];
  } catch {
    return [];
  }
}

interface ConvertQueueState {
  items: ConvertItem[];
  running: boolean;
  /** False (and nothing added) when this source is already waiting or building. */
  add: (item: NewConvertItem) => boolean;
  remove: (id: string) => void;
  move: (id: string, delta: -1 | 1) => void;
  /** Run pending items until none are left or Stop. */
  start: () => Promise<void>;
  /** Pause after the current build. */
  stop: () => void;
  /** Cancel the running build (it fails as cancelled; the queue goes on unless stopped). */
  cancelCurrent: () => Promise<void>;
  clearFinished: () => void;
}

let seq = 0;
let stopRequested = false;

export const useConvertQueue = create<ConvertQueueState>((set, get) => {
  const save = () => safeSetItem(KEY, JSON.stringify(get().items));
  const patch = (id: string, p: Partial<ConvertItem>) => {
    set({ items: get().items.map((i) => (i.id === id ? { ...i, ...p } : i)) });
    save();
  };

  return {
    items: typeof window === "undefined" ? [] : load(),
    running: false,
    add: (item) => {
      // The same game for the same console once; another console may have it too.
      const busy = get().items.some(
        (i) =>
          i.source === item.source &&
          hostOf(i.host ?? "") === hostOf(item.host ?? "") &&
          (i.status === "pending" || i.status === "running"),
      );
      if (busy) return false;
      set({ items: [...get().items, { ...item, id: `c${Date.now().toString(36)}${++seq}`, status: "pending" }] });
      save();
      return true;
    },
    remove: (id) => {
      if (get().items.find((i) => i.id === id)?.status === "running") return;
      set({ items: get().items.filter((i) => i.id !== id) });
      save();
    },
    move: (id, delta) => {
      const items = [...get().items];
      const at = items.findIndex((i) => i.id === id);
      const to = at + delta;
      if (at < 0 || to < 0 || to >= items.length) return;
      [items[at], items[to]] = [items[to], items[at]];
      set({ items });
      save();
    },
    start: async () => {
      if (get().running || !runner) return;
      stopRequested = false;
      set({ running: true });
      try {
        for (;;) {
          if (stopRequested) break;
          const next = get().items.find((i) => i.status === "pending");
          if (!next) break;
          patch(next.id, { status: "running", error: undefined });
          let r: { ok: boolean; packagePath?: string; message?: string; installed?: boolean };
          try {
            r = await runner.build(next);
          } catch (e) {
            r = { ok: false, message: e instanceof Error ? e.message : String(e) };
          }
          if (!r.ok || !r.packagePath) {
            patch(next.id, { status: "failed", error: r.message ?? "The build did not finish." });
            continue;
          }
          const pkg = r.packagePath;
          if (next.then === "keep" || !next.host || r.installed) {
            patch(next.id, { status: "done", packagePath: pkg });
            continue;
          }
          // Hand off and move on: the console queue runs the install.
          patch(next.id, { status: "installing", packagePath: pkg });
          void runner
            .handOff(next, pkg)
            .then((h) =>
              patch(next.id, h.ok ? { status: "done" } : { status: "failed", error: h.message ?? "The install did not finish." }),
            )
            .catch((e: unknown) => patch(next.id, { status: "failed", error: String(e) }));
        }
      } finally {
        set({ running: false });
      }
    },
    stop: () => {
      stopRequested = true;
    },
    cancelCurrent: async () => {
      await runner?.cancel();
    },
    clearFinished: () => {
      set({ items: get().items.filter((i) => i.status === "pending" || i.status === "running" || i.status === "installing") });
      save();
    },
  };
});
