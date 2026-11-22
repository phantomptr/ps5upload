// The Convert queue: games to build on this computer, one at a time (a build uses this
// computer's CPU and disk, not a console). When a build that should install finishes, its
// package is handed to the console queue and the next build starts — game 2 converts while game
// 1 installs. The running build is shown by the Convert screen's own stage view.

import { hostOf } from "../lib/addr";
import { create } from "zustand";

import type { AmprLz4Options, FfpfscLevel, FpkgCompression, ImageFormat, ZstdPacks } from "../api/fpkg";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useUploadSettingsStore } from "./uploadSettings";
import type { AmprEmuBuild, ZstdRuntime } from "./convertPrefs";

export type ConvertThen = "keep" | "stream" | "upload";
export type ConvertStatus = "pending" | "running" | "installing" | "handed" | "done" | "failed" | "cancelled";

/** What a queued game becomes: an installable package, a game image (compressed into a
 *  .ffpfsc when `compress`), or a plain folder (#469: from a game image or a debug-key
 *  package). An image's `lz4`: pack the game's data into asset packs first (LZ4, or zstd with
 *  `zstdLevel`), in any format, compressed or not. A folder's `zstd` / `lz4`: its files in zstd
 *  or LZ4 packs (then a game folder can be the source too). `unpackPacks`: a game in asset
 *  packs written as plain files again (a game folder can then be a folder's source too). */
export type ConvertBuild =
  | { kind: "pkg" }
  | { kind: "image"; format: ImageFormat; compress: boolean; lz4?: AmprLz4Options; unpackPacks?: boolean }
  | { kind: "folder"; zstd?: ZstdPacks; lz4?: AmprLz4Options; unpackPacks?: boolean };

/** The file a build makes, by its extension: what the queue row and the add button say. A
 *  folder has none; its row says "folder". */
export function buildExtension(build: ConvertBuild | undefined): string {
  if (!build || build.kind === "pkg") return ".pkg";
  if (build.kind === "folder") return "folder";
  return build.compress ? ".ffpfsc" : `.${build.format}`;
}

/** The image as a run makes it, with its asset packs when they are on: packs go into any image,
 *  compressed into a .ffpfsc or not. */
export function imageBuildWith(
  build: Extract<ConvertBuild, { kind: "image" }>,
  lz4: AmprLz4Options | undefined,
): Extract<ConvertBuild, { kind: "image" }> {
  return lz4 ? { ...build, lz4 } : build;
}

/** A build's asset packs, whichever field holds them (an image's `lz4` carries zstd packs too,
 *  a folder has `zstd` and `lz4`): what a queued game's options show and change (#905). */
export interface BuildPacks {
  kind: "none" | "lz4" | "zstd";
  /** The zstd level, or the LZ4 level when one was set. */
  level?: number;
  runtime?: ZstdRuntime;
  profileToml?: string;
  installRuntime?: boolean;
}

/** zstd packs are made at this level unless another was picked (Convert's ③ starts there). */
export const DEFAULT_ZSTD_LEVEL = 5;

/** An object without its undefined fields: what is saved and compared stays plain. */
function plain<T extends object>(o: T): T {
  return Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;
}

export function packsOf(build: ConvertBuild | undefined): BuildPacks {
  if (build?.kind === "image" && build.lz4) {
    const l = build.lz4;
    return l.zstdLevel
      ? plain({ kind: "zstd" as const, level: l.zstdLevel, runtime: l.zstdRuntime, profileToml: l.profileToml })
      : plain({ kind: "lz4" as const, level: l.level, profileToml: l.profileToml, installRuntime: l.installRuntime });
  }
  if (build?.kind === "folder" && build.zstd) {
    const z = build.zstd;
    return plain({ kind: "zstd" as const, level: z.level, runtime: z.runtime, profileToml: z.profileToml });
  }
  if (build?.kind === "folder" && build.lz4) {
    const l = build.lz4;
    return plain({ kind: "lz4" as const, level: l.level, profileToml: l.profileToml, installRuntime: l.installRuntime });
  }
  return { kind: "none" };
}

/** The build with these packs in place of its own (none: no packs). Packs and "write the packs
 *  out as plain files" are one choice, so turning packs on drops the latter; a package has none. */
export function withPacks(build: ConvertBuild, packs: BuildPacks): ConvertBuild {
  if (build.kind === "pkg") return build;
  const unpack = packs.kind === "none" && build.unpackPacks ? { unpackPacks: true } : {};
  const level = packs.level ?? DEFAULT_ZSTD_LEVEL;
  const lz4: AmprLz4Options | undefined =
    packs.kind === "lz4"
      ? plain({ level: packs.level, profileToml: packs.profileToml, installRuntime: packs.installRuntime })
      : undefined;
  if (build.kind === "image") {
    const packed =
      packs.kind === "zstd" ? plain({ zstdLevel: level, zstdRuntime: packs.runtime, profileToml: packs.profileToml }) : lz4;
    return { kind: "image", format: build.format, compress: build.compress, ...unpack, ...(packed ? { lz4: packed } : {}) };
  }
  return {
    kind: "folder",
    ...unpack,
    ...(packs.kind === "zstd" ? { zstd: plain({ level, runtime: packs.runtime, profileToml: packs.profileToml }) } : {}),
    ...(lz4 ? { lz4 } : {}),
  };
}

/** The few choices a queue row shows next to its file type, so a game set up wrongly is seen
 *  before the queue runs (#905): its asset packs, packs written out as plain files, a .ffpfsc's
 *  level and a package's one language. */
export type QueueTag =
  | { kind: "zstd"; level: number }
  | { kind: "lz4" }
  | { kind: "unpack" }
  | { kind: "level"; level: FfpfscLevel }
  | { kind: "language"; code: string };

export function queueTags(item: ConvertItem): QueueTag[] {
  const tags: QueueTag[] = [];
  const b = item.build;
  const packs = packsOf(b);
  if (packs.kind === "zstd") tags.push({ kind: "zstd", level: packs.level ?? DEFAULT_ZSTD_LEVEL });
  if (packs.kind === "lz4") tags.push({ kind: "lz4" });
  if ((b?.kind === "image" || b?.kind === "folder") && b.unpackPacks) tags.push({ kind: "unpack" });
  if (b?.kind === "image" && b.compress && item.imageLevel) tags.push({ kind: "level", level: item.imageLevel });
  if ((!b || b.kind === "pkg") && item.language) tags.push({ kind: "language", code: item.language });
  return tags;
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
  /** Leave the backport libraries in this game's fakelib out of its image or folder: ticked
   *  for this game when it was added (never set for a game that was not checked). */
  removeBackport?: boolean;
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
  /** `unverified`: the PS5 took the install but it could not be confirmed (package kept);
   *  `message` says what to check. */
  handOff(item: ConvertItem, packagePath: string): Promise<{ ok: boolean; unverified?: true; message?: string }>;
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
              patch(
                next.id,
                h.ok
                  ? // Unconfirmed is not a failure, but the row must say so: its note shows by the row.
                    { status: "done", ...(h.unverified && h.message ? { error: h.message } : {}) }
                  : { status: "failed", error: h.message ?? "The install did not finish." },
              ),
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
