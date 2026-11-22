import type { CleanupItem } from "../api/cleanup";
import type { InstalledPkgArtifact, InstalledTitle, LibraryEntry, Volume } from "../api/ps5";
import { volumeLikelyFitsBytes } from "../api/ps5";
import { smpMountImageBasename } from "../lib/mountPaths";
import { buildCandidates, isInternalPath, sameDrive, type SpaceCandidate } from "../lib/spacePlan";

/**
 * Reads what a space plan is made from: the console's drives, its Library, its installed games
 * (with where each one's package is), and the sizes of the game folders that could move. The
 * clean-up scan is the Clean up screen's own, passed in.
 */

export interface GatherDeps {
  volumes: () => Promise<Volume[]>;
  library: () => Promise<LibraryEntry[]>;
  installed: () => Promise<InstalledTitle[]>;
  inventory: (titleId: string) => Promise<InstalledPkgArtifact[]>;
  /** A folder's recursive size, null when it could not be read in full. */
  folderBytes: (path: string) => Promise<number | null>;
  /** What Kraken Mount uses on the console (#894); left out = nothing is checked. */
  krakenKeeps?: () => Promise<(path: string, isDir?: boolean) => boolean>;
}

export interface GatherInput {
  target: string;
  leftovers: CleanupItem[];
  collectionIds: Set<string>;
  running: Set<string>;
  lastPlayed: (titleId: string | null | undefined) => number | null;
}

export interface Gathered {
  candidates: SpaceCandidate[];
  /** The target drive's room now, as the console reports it. */
  roomBytes: number;
  /** Room on every other drive. */
  destRoom: Record<string, number>;
  volumes: string[];
  /** Game folders whose size could not be read (left out of the plan). */
  unmeasured: number;
}

/** The writable drives a plan can use, image mounts apart. */
export function planVolumes(vols: Volume[]): { drives: Volume[]; imageMounts: string[] } {
  const real = vols.filter((v) => v.writable && !v.is_placeholder);
  const isImage = (v: Volume) =>
    !!v.source_image || v.path.startsWith("/mnt/ps5upload/") || !!smpMountImageBasename(v.path);
  return {
    drives: real.filter((v) => !isImage(v)),
    imageMounts: vols.filter(isImage).map((v) => v.path),
  };
}

export async function gatherSpacePlan(
  input: GatherInput,
  deps: GatherDeps,
  onPhase: (phase: "drives" | "games" | "sizes", name?: string) => void = () => {},
): Promise<Gathered> {
  onPhase("drives");
  const all = await deps.volumes();
  const { drives, imageMounts } = planVolumes(all);
  const volumes = drives.map((v) => v.path);
  const target = drives.find((v) => sameDrive(v.path, input.target));
  const roomBytes = target ? volumeLikelyFitsBytes(target) : 0;
  const destRoom: Record<string, number> = {};
  for (const v of drives) {
    // The internal drive can be listed twice (/user and /data): count it once.
    if (sameDrive(v.path, input.target)) continue;
    if (isInternalPath(v.path) && Object.keys(destRoom).some(isInternalPath)) continue;
    destRoom[v.path] = volumeLikelyFitsBytes(v);
  }

  onPhase("games");
  const [library, installed, keeps] = await Promise.all([
    deps.library(),
    deps.installed(),
    deps.krakenKeeps ? deps.krakenKeeps() : Promise.resolve(undefined),
  ]);
  const inUsePaths = new Set<string>();
  const imageNames = new Set<string>();
  for (const t of installed) {
    if (!t.source) continue;
    inUsePaths.add(t.source);
    const smp = smpMountImageBasename(t.source);
    if (smp) imageNames.add(smp);
  }
  for (const v of all) if (v.source_image) inUsePaths.add(v.source_image);

  // Only a game that could be uninstalled needs its package located: one with a copy here.
  const pkgTitles = installed.filter(
    (t) => t.origin === "pkg" && !t.system && input.collectionIds.has(t.titleId.toUpperCase()),
  );
  const bases = new Map<string, { path: string; size: number } | null>();
  for (const t of pkgTitles) {
    try {
      const arts = await deps.inventory(t.titleId);
      const base = arts.find((a) => a.kind === "base" && a.size > 0);
      bases.set(t.titleId, base ? { path: base.path, size: base.size } : null);
    } catch {
      bases.set(t.titleId, null);
    }
  }

  const build = (folderBytes: Record<string, number>) =>
    buildCandidates({
      target: target?.path ?? input.target,
      volumes,
      imageMounts,
      leftovers: input.leftovers,
      library,
      folderBytes,
      inUse: { paths: inUsePaths, imageNames },
      installed: installed.map((t) => ({
        titleId: t.titleId,
        titleName: t.titleName,
        origin: t.origin,
        system: t.system,
        base: bases.get(t.titleId) ?? null,
      })),
      collectionIds: input.collectionIds,
      running: input.running,
      lastPlayed: input.lastPlayed,
      ...(keeps ? { krakenKeeps: (p: string, isDir: boolean) => keeps(p, isDir) } : {}),
    });

  // Measure only the folders that could move: a folder's own entry says 0 bytes.
  const folders = new Set(library.filter((e) => e.kind === "game").map((e) => e.path));
  const probe = build(Object.fromEntries([...folders].map((p) => [p, 1])));
  const toMeasure = probe.filter((c) => c.kind === "move" && c.path && folders.has(c.path));
  const sizes: Record<string, number> = {};
  let unmeasured = 0;
  for (const c of toMeasure) {
    onPhase("sizes", c.label);
    const n = await deps.folderBytes(c.path as string).catch(() => null);
    if (n != null && n > 0) sizes[c.path as string] = n;
    else unmeasured++;
  }
  return { candidates: build(sizes), roomBytes, destRoom, volumes, unmeasured };
}
