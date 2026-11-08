// The Convert queue: games to build on this computer, one at a time (a build uses this
// computer's CPU and disk, not a console). When a build that should install finishes, its
// package is handed to the console queue and the next build starts — game 2 converts while game
// 1 installs. The running build is shown by the Convert screen's own stage view.

import { hostOf } from "../lib/addr";
import { create } from "zustand";

import type { AmprLz4Options, FfpfscLevel, FpkgCompression, ImageFormat, ZstdPacks } from "../api/fpkg";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useUploadSettingsStore } from "./uploadSettings";
import type { AmprEmuBuild } from "./convertPrefs";

export type ConvertThen = "keep" | "stream" | "upload";
export type ConvertStatus = "pending" | "running" | "installing" | "handed" | "done" | "failed" | "cancelled";

/** What a queued game becomes: an installable package, a game image (compressed into a
 *  .ffpfsc when `compress`), or a plain folder (#469: from a game image or a debug-key
 *  package). `lz4`: pack the game's data into AMPR LZ4 asset packs (an .exfat of a game that
 *  reads through libSceAmpr). A folder's `zstd`: its files in zstd packs (then a game folder
 *  can be the source too). */
export type ConvertBuild =
  | { kind: "pkg" }
  | { kind: "image"; format: ImageFormat; compress: boolean; lz4?: AmprLz4Options }
  | { kind: "folder"; zstd?: ZstdPacks };

/** The file a build makes, by its extension: what the queue row and the add button say. A
 *  folder has none; its row says "folder". */
export function buildExtension(build: ConvertBuild | undefined): string {
  if (!build || build.kind === "pkg") return ".pkg";
  if (build.kind === "folder") return "folder";
  return build.compress && !build.lz4 ? ".ffpfsc" : `.${build.format}`;
}

/** The image as a run makes it: LZ4 asset packs go into a plain image, so a remembered "compress"
 *  does not apply to one (the engine refuses the two together). */
export function imageBuildWith(
  build: Extract<ConvertBuild, { kind: "image" }>,
  lz4: AmprLz4Options | undefined,
): Extract<ConvertBuild, { kind: "image" }> {
  return lz4 ? { ...build, compress: false, lz4 } : build;
}

/** What is done after converting, as it can be done: an image is never streamed (it is
 *  uploaded), a folder is only kept, and nothing is sent anywhere while no console answers. */
export function thenFor(kind: ConvertBuild["kind"], then: ConvertThen, canInstall: boolean): ConvertThen {
  if (!canInstall || kind === "folder") return "keep";
  return kind === "image" && then === "stream" ? "upload" : then;
}

export interface ConvertItem {
  id: string;
  /** A game folder, image or archive, here or on a server or console. */
  source: string;
  /** The source is a folder, whatever its name looks like (`Some.Game.2024`). */
  isDir?: boolean;
  /** A .rar's password: in this window's memory only, never saved with the queue. */
  password?: string;
  outputDir?: string;
  /** The output goes next to its source (Workshop's "Save each next to its source"): a name
   *  already taken there is numbered, never overwritten or refused. */
  keepBoth?: boolean;
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
  /** Re-read every byte of the output to check it, as it was set when the game was added
   *  (absent on items queued before this was per game: the current setting). */
  fullVerify?: boolean;
  /** How hard a .ffpfsc is compressed, likewise. */
  imageLevel?: FfpfscLevel;
  /** An image of a libSceAmpr game with no emulator gets the AMPR emulator, and which one: as
   *  set when the game was added (absent on older items: the current settings at run time). */
  addAmprEmu?: boolean;
  amprEmuBuild?: AmprEmuBuild;
  /** Save to a server: the image or folder goes straight to this `remote://<connection>/<folder>`
   *  path, as chosen when the game was added (never kept on this computer). */
  toServer?: string;
  status: ConvertStatus;
  packagePath?: string;
  error?: string;
}

export type NewConvertItem = Omit<ConvertItem, "id" | "status" | "packagePath" | "error">;

/** What can change on a game still waiting in the queue: every choice it was added with (#669),
 *  not only what it becomes. */
export type ConvertItemChange = Partial<
  Pick<
    ConvertItem,
    | "build"
    | "then"
    | "host"
    | "deleteAfterInstall"
    | "imageDest"
    | "outputDir"
    | "keepBoth"
    | "compression"
    | "language"
    | "fullVerify"
    | "imageLevel"
  >
>;

/** How the queue builds and installs; the app wires the real ones (see convertQueueRunner). */
export interface ConvertRunner {
  /** `installed`: the build also installed it (a console dump's swap runs inside the build). */
  build(item: ConvertItem): Promise<{ ok: boolean; packagePath?: string; message?: string; installed?: boolean; cancelled?: boolean }>;
  handOff(item: ConvertItem, packagePath: string): Promise<{ ok: boolean; message?: string }>;
  /** Resolves once this computer can start the next build (a build started by hand finishes
   *  first). The queue waits here before it takes the next game, so a game waiting behind a
   *  hand-started build still says Waiting, and can be removed, changed or stopped. */
  ready?(): Promise<void>;
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

/** What is written to storage: everything but a .rar's password. */
export function savedItems(items: ConvertItem[]): Omit<ConvertItem, "password">[] {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  return items.map(({ password, ...kept }) => kept);
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
  /** Games added during a build started by hand, with "Start the queue when you add something"
   *  on: the queue starts once that build's result is put away (Convert another, or a new
   *  source), so the result's buttons stay until then; never after a build that failed. Start
   *  starts it at once. */
  armed: boolean;
  /** False (and nothing added) when this source is already waiting or building. */
  add: (item: NewConvertItem) => boolean;
  remove: (id: string) => void;
  /** Change what a game still waiting becomes or what happens after (#518): queued games stay
   *  editable while the queue runs. False when it is no longer waiting. */
  update: (id: string, change: ConvertItemChange) => boolean;
  /** A .rar refused for its password: try it again with this one, starting the queue. */
  retryWithPassword: (id: string, password: string) => void;
  move: (id: string, delta: -1 | 1) => void;
  /** Run pending items until none are left or Stop; games added meanwhile are taken in order. */
  start: () => Promise<void>;
  /** Pause after the current build. */
  stop: () => void;
  /** Hold the games still waiting: the running build is cancelled by the caller. */
  holdNext: () => void;
  clearFinished: () => void;
}

let seq = 0;
let stopRequested = false;

/** A game added while a build runs on this computer (#621): it is made after that build, in
 *  order, without pressing Start — as Upload & Install's "Start the queue when you add
 *  something" does for the console's queue, and only when that is on. A queue already running
 *  takes it by itself; with nothing building, adding only adds, as before. The queue does not
 *  start the moment the hand-started build ends: its result (Install, Launch, Delete, Retry…)
 *  stays until it is put away (see `pipelinePhaseChanged`). */
export function startAfterBuild(building: boolean): void {
  if (!building || !useUploadSettingsStore.getState().queueAutoStart) return;
  const q = useConvertQueue.getState();
  if (q.running || !q.items.some((i) => i.status === "pending")) return;
  useConvertQueue.setState({ armed: true });
}

/** The Convert pipeline's phase, as it changes: an armed queue starts once the hand-started
 *  build's result is put away (idle), and is disarmed when that build failed (its Retry and
 *  message stay; the user starts the queue). */
export function pipelinePhaseChanged(phase: string): void {
  if (!useConvertQueue.getState().armed) return;
  if (phase === "failed") useConvertQueue.setState({ armed: false });
  else if (phase === "idle") void useConvertQueue.getState().start();
}

export const useConvertQueue = create<ConvertQueueState>((set, get) => {
  const save = () => safeSetItem(KEY, JSON.stringify(savedItems(get().items)));
  const patch = (id: string, p: Partial<ConvertItem>) => {
    set({ items: get().items.map((i) => (i.id === id ? { ...i, ...p } : i)) });
    save();
  };

  return {
    items: typeof window === "undefined" ? [] : load(),
    running: false,
    armed: false,
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
    update: (id, change) => {
      if (get().items.find((i) => i.id === id)?.status !== "pending") return false;
      patch(id, change);
      return true;
    },
    retryWithPassword: (id, password) => {
      if (get().items.find((i) => i.id === id)?.status !== "failed") return;
      patch(id, { status: "pending", error: undefined, password });
      void get().start();
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
      set({ armed: false });
      if (get().running || !runner) return;
      const r0 = runner;
      stopRequested = false;
      set({ running: true });
      try {
        for (;;) {
          if (stopRequested) break;
          // A build started by hand finishes first; only then is the next game taken (it may
          // have been removed, changed or stopped meanwhile).
          if (r0.ready) await r0.ready();
          if (stopRequested) break;
          const next = get().items.find((i) => i.status === "pending");
          if (!next) break;
          patch(next.id, { status: "running", error: undefined });
          let r: { ok: boolean; packagePath?: string; message?: string; installed?: boolean; cancelled?: boolean };
          try {
            r = await r0.build(next);
          } catch (e) {
            r = { ok: false, message: e instanceof Error ? e.message : String(e) };
          }
          if (r.cancelled) {
            patch(next.id, { status: "cancelled", password: undefined });
            continue;
          }
          if (!r.ok || !r.packagePath) {
            patch(next.id, { status: "failed", error: r.message ?? "The build did not finish." });
            continue;
          }
          patch(next.id, { password: undefined });
          const pkg = r.packagePath;
          if (next.then === "keep" || !next.host || r.installed) {
            patch(next.id, { status: "done", packagePath: pkg });
            continue;
          }
          // Hand off and move on: the console queue runs the install.
          patch(next.id, { status: "installing", packagePath: pkg });
          void r0
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
      set({ armed: false });
    },
    holdNext: () => {
      stopRequested = true;
      set({ armed: false });
    },
    clearFinished: () => {
      set({ items: get().items.filter((i) => i.status === "pending" || i.status === "running" || i.status === "installing") });
      save();
    },
  };
});
