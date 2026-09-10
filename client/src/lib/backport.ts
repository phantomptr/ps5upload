import type { FsDirEntry, InstalledTitle, SdkTitle } from "../api/ps5";
import { parsePS5Firmware } from "./ps5Firmware";
import { sdkHexToFw } from "./sdkVersionHex";

/* Backporting replaces system libraries a game needs with ones the console's
 * older firmware does not have, in a `fakelib/` folder the loader overlays onto
 * `common/lib`. The libraries come from games that already ship them.
 *
 * The central constraint, established on hardware: a SET — the exact set
 * one real game carries — is the only unit that can be installed safely.
 * Combinations that no game has ever shipped are untested by anyone, and one
 * synthesised from the "newest build of each name" stopped a working title
 * (Red Dead Redemption) from launching at all. Nor can a set be chosen
 * from metadata: within a single declared SDK major, libSceAgc has four
 * distinct builds, and a library's own embedded SDK pair records only whether
 * its ripper patched it. Nothing in the data predicts which set a given
 * game needs.
 *
 * So this module does not try to be clever. It ranks plausible sets, makes
 * installing one atomic, and makes undo exact. Being wrong is expected; being
 * unable to go back is not. */

export interface BackportLibrary {
  name: string;
  size: number;
  sha256?: string;
  /** Location of the build inside the corpus, relative to manifest.json —
   *  e.g. "builds/libSceAgc/cd17870c.sprx". Present for corpus libraries;
   *  absent for ones read off the console. */
  path?: string;
  /** How many corpus titles ship this exact build. The only signal with
   *  evidence behind it when choosing between builds. */
  shippedBy?: number;
}

/** One distinct build of a library, stored once and addressed by content.
 *
 *  The corpus is small and heavily shared — 13 library names, ~52 builds,
 *  across ~34 games — so the build, not the game, is the unit worth storing.
 *  `shippedBy` is the evidence for choosing between builds of a name: one that
 *  13 games ship is better travelled than a singleton. */
export interface FakelibBuild {
  sha256: string;
  size: number;
  sdk: number[] | null;
  shippedBy: string[];
  path: string;
}

export interface FakelibLibraryEntry {
  name: string;
  builds: FakelibBuild[];
}

/** What one real game ships, as references into the build store. A combination
 *  known to work somewhere — recorded as metadata, duplicating no bytes. */
export interface ObservedSet {
  title_id: string;
  title_name: string;
  sdk_version: string;
  image_backed: boolean;
  /** library name -> sha256 of the build that game ships. */
  libraries: Record<string, string>;
}

/** Where a set came from. A pack the user imported and a game harvested from a
 *  console are different kinds of evidence, and the UI has to say which. */
export type SetOrigin =
  | { kind: "scan"; title_id: string; console?: string; at?: string }
  | { kind: "import"; source?: string; at?: string };

/** One library set, kept whole.
 *
 *  Whole is the point: a set is only known to work as the unit somebody
 *  actually shipped. Installing a mixture of two makes a combination nobody has
 *  run. */
export interface FakelibSet {
  id: string;
  label: string;
  origin: SetOrigin;
  libraries: BackportLibrary[];
}

/** The title a scanned set was harvested from, if it was scanned at all. */
export function sourceTitleId(set: FakelibSet): string | null {
  return set.origin.kind === "scan" ? set.origin.title_id : null;
}

export interface FakelibManifest {
  schema: number;
  libraries: FakelibLibraryEntry[];
  observed_sets: ObservedSet[];
}

/* A corpus is read off disk and may be hand-edited, so every field is checked
 * before use. `path` in particular becomes part of a filesystem path we copy
 * from, so it must not escape the corpus directory. */
const LIBRARY_NAME = /^[^.][^/\\]*\.(?:sprx|prx)$/i;
const BUILD_PATH = /^builds\/[A-Za-z0-9_.-]+\/[A-Za-z0-9]+\.(?:sprx|prx)$/i;
const SHA256 = /^[0-9a-f]{64}$/i;

/** Turn a content-addressed manifest into installable sets.
 *
 *  Drops any set referencing a build the corpus does not hold, or whose fields
 *  do not validate. A half-resolved set would install some libraries and
 *  silently skip others, producing a combination no game ever shipped — which
 *  is the one thing the corpus exists to prevent. */
export function resolveSets(manifest: unknown): FakelibSet[] {
  if (!manifest || typeof manifest !== "object") return [];
  const m = manifest as Record<string, unknown>;
  if (!Array.isArray(m.libraries) || !Array.isArray(m.sets)) return [];

  const bySha = new Map<string, { name: string; size: number; path: string; shipped: number }>();
  for (const entry of m.libraries) {
    if (!entry || typeof entry !== "object") continue;
    const lib = entry as Record<string, unknown>;
    if (typeof lib.name !== "string" || !LIBRARY_NAME.test(lib.name)) continue;
    if (!Array.isArray(lib.builds)) continue;
    for (const raw of lib.builds) {
      if (!raw || typeof raw !== "object") continue;
      const b = raw as Record<string, unknown>;
      if (typeof b.sha256 !== "string" || !SHA256.test(b.sha256)) continue;
      if (typeof b.size !== "number" || !Number.isSafeInteger(b.size) || b.size < 0) continue;
      if (typeof b.path !== "string" || !BUILD_PATH.test(b.path)) continue;
      const shipped = Array.isArray(b.shipped_by) ? b.shipped_by.length : 0;
      bySha.set(b.sha256.toLowerCase(), { name: lib.name, size: b.size, path: b.path, shipped });
    }
  }

  const out: FakelibSet[] = [];
  for (const value of m.sets) {
    if (!value || typeof value !== "object") continue;
    const set = value as Record<string, unknown>;
    if (typeof set.id !== "string" || !set.id) continue;
    if (!set.libraries || typeof set.libraries !== "object") continue;
    const origin = parseOrigin(set.origin);
    if (!origin) continue;

    const wanted = Object.entries(set.libraries as Record<string, unknown>);
    const libraries: BackportLibrary[] = [];
    for (const [name, sha] of wanted) {
      if (!LIBRARY_NAME.test(name) || typeof sha !== "string") break;
      const hit = bySha.get(sha.toLowerCase());
      // The manifest must agree with itself: a build filed under one name
      // cannot be installed as another.
      if (!hit || hit.name !== name) break;
      libraries.push({ name, size: hit.size, sha256: sha.toLowerCase(), path: hit.path,
                       shippedBy: hit.shipped });
    }
    if (libraries.length !== wanted.length || libraries.length === 0) continue;

    libraries.sort((a, b) => a.name.localeCompare(b.name));
    const label = typeof set.label === "string" && set.label.trim() ? set.label : set.id;
    out.push({ id: set.id, label, origin, libraries });
  }
  return out;
}

function parseOrigin(value: unknown): SetOrigin | null {
  if (!value || typeof value !== "object") return null;
  const o = value as Record<string, unknown>;
  if (o.kind === "scan" && typeof o.title_id === "string") {
    return { kind: "scan", title_id: o.title_id,
             console: typeof o.console === "string" ? o.console : undefined,
             at: typeof o.at === "string" ? o.at : undefined };
  }
  if (o.kind === "import") {
    return { kind: "import",
             source: typeof o.source === "string" ? o.source : undefined,
             at: typeof o.at === "string" ? o.at : undefined };
  }
  return null;
}


export interface BackportCopy extends BackportLibrary {
  from: string;
  to: string;
}

export interface BackportPlan {
  target: InstalledTitle;
  set: FakelibSet;
  /** Libraries to write into the target's fakelib/. */
  copies: BackportCopy[];
  /** Libraries already present that the install will remove. A set is
   *  installed WHOLE, so anything not in it goes — but it is stashed on the
   *  console first (see `stashDir`) so undo can put it back exactly. */
  replaced: BackportLibrary[];
  /** Where `replaced` libraries are copied before deletion. Lives under /data,
   *  which is always writable — the target's own fakelib/ may sit inside a
   *  read-only disk image that is only writable during the edit session. */
  stashDir: string;
}

export interface BackportRecord {
  titleId: string;
  setId: string;
  targetSource: string;
  copiedPaths: string[];
  /** What the target's fakelib/ held before, for exact restoration. */
  replaced: BackportLibrary[];
  stashDir: string;
  /** False when apply threw partway: the SDK is patched and some libraries
   *  landed, which launches and then aborts. Undo must still be offered. */
  complete: boolean;
}

export interface BackportTransport {
  /** Copy between two paths that both live on the PS5. */
  copyConsole(from: string, to: string): Promise<void>;
  /** Transfer one file from the user's computer into the PS5. */
  uploadHost(from: string, to: string): Promise<void>;
  mkdirConsole(path: string): Promise<void>;
  patch(titleId: string, patchLibc: boolean): Promise<void>;
  restore(titleId: string): Promise<void>;
  remove(path: string): Promise<void>;
}

export class BackportApplyError extends Error {
  copiedPaths: string[];
  replaced: BackportLibrary[];

  constructor(
    message: string,
    copiedPaths: string[],
    replaced: BackportLibrary[],
  ) {
    super(message);
    this.name = "BackportApplyError";
    this.copiedPaths = copiedPaths;
    this.replaced = replaced;
  }
}

/* A dot-prefixed name is never a library. Copying a game folder from a Mac
 * leaves an AppleDouble sidecar (`._libSceAgc.sprx`, always ~4 KB) beside
 * every file, and those end in .sprx like the real thing — the first build of
 * this offered eight of them for installation. `.DS_Store` is the same class
 * of noise. */
const isLibrary = (entry: FsDirEntry) =>
  entry.kind === "file" &&
  !entry.name.startsWith(".") &&
  /\.(?:sprx|prx)$/i.test(entry.name);

export function existingLibraries(entries: FsDirEntry[]): BackportLibrary[] {
  return entries
    .filter(isLibrary)
    .map(({ name, size }) => ({ name, size }))
    .sort((a, b) => a.name.localeCompare(b.name));
}

/** The SDK a title was BUILT against — the only field that predicts whether it
 *  runs on older firmware.
 *
 *  `requiredSystemSoftwareVersion` does not: SILENT HILL 2 declares 10.20, was
 *  built with 9.00, and runs on a 9.60 console with no fakelib at all. Across
 *  31 image-backed titles, every one with sdkVersion above the console
 *  firmware ships a fakelib and every one below it does not — no exceptions. */
export function requiresBackport(
  titleSdkVersion: string,
  consoleKernel: string | null | undefined,
): boolean {
  const required = sdkHexToFw(titleSdkVersion);
  const current = parsePS5Firmware(consoleKernel);
  if (!required || !current) return false;
  const numeric = (value: string) => {
    const [major, minor] = value.split(".").map(Number);
    return major * 100 + minor;
  };
  return numeric(required) > numeric(current);
}

export function isBackportEligible(
  title: InstalledTitle,
  scan: SdkTitle | undefined,
  consoleKernel: string | null | undefined,
): boolean {
  return !!title.source && !!scan?.patchable &&
    requiresBackport(scan.sdk_version, consoleKernel);
}


/** Sets worth trying for `target`, best guess first.
 *
 *  Ordering only: none of these signals is known to predict success, so the UI
 *  must let the user work down the list rather than presenting the head of it
 *  as an answer. Same declared SDK major first (a weak but real correlation),
 *  then FEWEST libraries — adding libraries a game does not need is what broke
 *  Red Dead, so the least intrusive candidate is tried first. `exclude` drops
 *  sets already tried and rejected for this title. */
/** How well attested a set is: the count of corpus titles shipping its RAREST
 *  build. A set is only as well-travelled as its least common member. */
export function setAttestation(set: FakelibSet): number {
  return set.libraries.reduce(
    (worst, lib) => Math.min(worst, lib.shippedBy ?? 0),
    Number.POSITIVE_INFINITY,
  );
}

export function rankSets(
  sets: FakelibSet[],
  exclude: string[] = [],
  targetTitleId?: string,
): FakelibSet[] {
  const skip = new Set(exclude);
  return sets
    .filter((p) => !skip.has(p.id) && p.libraries.length > 0)
    .sort(
      (a, b) =>
        // The target's own set, when the corpus has it, is the one combination
        // known to work for this exact game.
        Number(sourceTitleId(b) === targetTitleId) -
          Number(sourceTitleId(a) === targetTitleId) ||
        // Then attestation. Measured across 34 titles, the target's SDK does
        // NOT predict the build: one libSceAgc build ships in titles declaring
        // SDK 0500, 0900, 1000 and 1100, while a single SDK (1100) uses four
        // different builds. Sorting on SDK major, which this used to do, was
        // ordering on noise. Popularity is the only signal with evidence
        // behind it — a combination 13 games run beats a singleton.
        setAttestation(b) - setAttestation(a) ||
        // Then least intrusive: fewer libraries means fewer chances to hand a
        // game something it never asked for.
        a.libraries.length - b.libraries.length ||
        a.id.localeCompare(b.id),
    );
}


/** Install `set` into `target` WHOLE: every library the set has, and
 *  nothing else. Existing libraries are recorded as `replaced` so undo can
 *  restore the title byte-for-byte. */
export const STASH_ROOT = "/data/ps5upload/backport";

export function planBackport(
  target: InstalledTitle,
  set: FakelibSet,
  existing: BackportLibrary[],
  corpusRoot: string,
): BackportPlan {
  if (set.libraries.length === 0) {
    throw new Error(`Set ${set.id} has no libraries`);
  }
  const unsafe = set.libraries.find(
    (lib) => lib.name.startsWith(".") || !/^[^/\\]+\.(?:sprx|prx)$/i.test(lib.name),
  );
  if (unsafe) throw new Error(`Set ${set.id} has unsafe library name: ${unsafe.name}`);
  return {
    target,
    set,
    copies: set.libraries.map((lib) => {
      if (!lib.path) {
        throw new Error(`Library ${lib.name} has no corpus path`);
      }
      return {
        ...lib,
        from: `${corpusRoot}/${lib.path}`,
        to: `${target.source}/fakelib/${lib.name}`,
      };
    }),
    // Stash every original, including same-named files. Hardware proved that
    // sets can share names and sizes while containing different builds.
    replaced: existing,
    stashDir: `${STASH_ROOT}/${target.titleId}`,
  };
}

export async function applyBackport(
  plan: BackportPlan,
  transport: BackportTransport,
  patchLibc: boolean,
): Promise<BackportRecord> {
  const copiedPaths: string[] = [];
  const replaced: BackportLibrary[] = [];
  try {
    if (plan.replaced.length > 0) await transport.mkdirConsole(plan.stashDir);
    for (const lib of plan.replaced) {
      const live = `${plan.target.source}/fakelib/${lib.name}`;
      await transport.copyConsole(live, `${plan.stashDir}/${lib.name}`);
      await transport.remove(live);
      replaced.push(lib);
    }
    await transport.mkdirConsole(`${plan.target.source}/fakelib`);
    for (const copy of plan.copies) {
      await transport.uploadHost(copy.from, copy.to);
      copiedPaths.push(copy.to);
    }
    // Patch last. A missing/unreadable corpus file must never leave behind an
    // SDK-only change that can pass Sony's launch gate and abort later.
    await transport.patch(plan.target.titleId, patchLibc);
  } catch (error) {
    throw new BackportApplyError(
      error instanceof Error ? error.message : String(error),
      copiedPaths,
      replaced,
    );
  }
  return {
    titleId: plan.target.titleId,
    setId: plan.set.id,
    targetSource: plan.target.source,
    copiedPaths,
    replaced,
    stashDir: plan.stashDir,
    complete: true,
  };
}

/** Overlay states the payload reports. `blocked` means an external BackPork
 *  already holds the mount: two overlays on one target make the kernel refuse
 *  the second (EDEADLK), and the game then starts without its libraries. */
export type BackportOverlayState = "idle" | "watching" | "mounted" | "blocked";

export function backportOverlayReady(
  status: { state?: string } | null | undefined,
): boolean {
  return status?.state === "watching" || status?.state === "mounted";
}

/** Why a backported title failed to launch.
 *
 *  Both outcomes look identical from the process table — no process — but they
 *  need opposite fixes, so guessing wastes a ~3 minute edit cycle each time.
 *  Measured on Red Dead Redemption: stripped of every library it emitted one
 *  `Call to unpatched function is detected!!!` line; loaded with a 13-library
 *  synthetic set it emitted none and simply died after `createApp`. */
export type LaunchDiagnosis = "missing-libraries" | "wrong-libraries" | "unknown";

export function diagnoseLaunchFailure(
  klog: string,
  titleId: string,
): LaunchDiagnosis {
  if (/call to unpatched function/i.test(klog)) return "missing-libraries";
  // Only claim "wrong" if the game actually got as far as being created —
  // otherwise the launch failed for some unrelated reason and neither answer
  // would help.
  const created = new RegExp(`createApp\\s+${titleId}`, "i").test(klog);
  return created ? "wrong-libraries" : "unknown";
}

/** What a verification run concluded. */
export type VerifyVerdict =
  | { kind: "missing-libraries" }
  | { kind: "wrong-libraries" }
  | { kind: "running"; peakThreads: number }
  | { kind: "unknown" };

export interface VerifySample {
  /** Threads reported for the title, or null when it has no process. */
  threads: number | null;
}

/** Decide what a launch showed.
 *
 *  Deliberately refuses to call a running process a success. Thread count
 *  misled three separate times (1 / 18 / 263 threads on titles whose real state
 *  was the opposite), and a trial that installed byte-identical libraries twice
 *  got a failure and a success. A live process means "ask the human to look at
 *  the screen", never "it worked".
 *
 *  Failure, by contrast, is sound: no process at all after the window, with
 *  klog saying whether the libraries were missing or merely wrong. */
export function verdictFrom(
  samples: VerifySample[],
  klog: string,
  titleId: string,
): VerifyVerdict {
  // Judged on the END of the window, not on whether a process was ever seen.
  // A title that starts and dies is a failure that happens to have had threads
  // — reading "peak 39 threads" from a run that was over by the end is exactly
  // the mistake that made thread count useless three times. Games also start
  // slowly (a cold start from USB showed nothing for the first 40 seconds), so
  // the last sample is the one that means anything.
  const last = samples[samples.length - 1];
  if (last && last.threads !== null) {
    const peakThreads = samples.reduce((max, s) => Math.max(max, s.threads ?? 0), 0);
    return { kind: "running", peakThreads };
  }
  const diagnosis = diagnoseLaunchFailure(klog, titleId);
  return diagnosis === "unknown" ? { kind: "unknown" } : { kind: diagnosis };
}

/** How many launches a failure must survive before it is believed.
 *
 *  Measured on hardware: Red Dead, unchanged between runs, launched, launched,
 *  then produced no process at all — 1 failure in 3 with nothing altered. A
 *  single failed launch is therefore not evidence about libraries, and acting
 *  on one sends the user through a pointless install-launch-undo cycle. */
export const FAILURE_ATTEMPTS = 3;

/** Fold repeated launch attempts into one verdict.
 *
 *  Any attempt that ends with the title up wins outright: launching is flaky in
 *  the failing direction, not the succeeding one — a game cannot run by
 *  accident.
 *
 *  `missing-libraries` is believed from a single attempt because it rests on an
 *  actual kernel message ("Call to unpatched function"), which is a positive
 *  signal rather than an absence. Everything else needs every attempt to agree.
 */
export function combineAttempts(attempts: VerifyVerdict[]): VerifyVerdict {
  const running = attempts.find((v) => v.kind === "running");
  if (running) return running;
  const missing = attempts.find((v) => v.kind === "missing-libraries");
  if (missing) return missing;
  if (attempts.length < FAILURE_ATTEMPTS) return { kind: "unknown" };
  const first = attempts[0] ?? { kind: "unknown" as const };
  return attempts.every((v) => v.kind === first.kind) ? first : { kind: "unknown" };
}

/** Sets worth offering after a verdict, best first.
 *
 *  A missing-library failure wants MORE libraries, so sets smaller than the one
 *  that failed cannot help and are dropped. A wrong-library failure wants a
 *  different lineage, so the ranking's own order stands. */
export function nextSetsAfter(
  verdict: VerifyVerdict,
  failed: FakelibSet,
  ranked: FakelibSet[],
): FakelibSet[] {
  const remaining = ranked.filter((s) => s.id !== failed.id);
  if (verdict.kind === "missing-libraries") {
    return remaining.filter((s) => s.libraries.length > failed.libraries.length);
  }
  return remaining;
}

export async function undoBackport(
  record: BackportRecord,
  transport: BackportTransport,
): Promise<void> {
  for (const path of [...record.copiedPaths].reverse()) {
    await transport.remove(path);
  }
  // Put back what the install displaced, from the stash, before reverting the
  // SDK — a title left with neither its own libraries nor ours launches and
  // aborts, which looks exactly like a failed backport rather than a failed
  // undo.
  for (const lib of record.replaced) {
    await transport.copyConsole(
      `${record.stashDir}/${lib.name}`,
      `${record.targetSource}/fakelib/${lib.name}`,
    );
  }
  await transport.restore(record.titleId);
  // Cleanup is non-semantic: once the original libraries and SDK are back,
  // failing to remove the backup must not leave a record whose next retry
  // deletes already-restored files and then looks for a missing stash.
  try {
    await transport.remove(record.stashDir);
  } catch {
    // A leftover /data backup is recoverable and safer than a false failed Undo.
  }
}
