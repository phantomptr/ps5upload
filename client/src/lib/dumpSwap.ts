// Convert & replace: swap a game dump on the console for the package built from it, reversibly.
//
// A dump and its package share a title id (saves and trophies depend on it), so they cannot be
// registered side by side, and ShadowMountPlus re-registers a dump over an install if it still
// sees it (measured on FW 5.10). So the dump is moved out of every scan root first ("parked",
// on its own volume — never a rename across mounts), SMP lets go, any leftover registration is
// removed, and only then does the package install. A failed install moves the dump back, and
// SMP re-registers it: the console is as it was.
//
// Every step is journaled on the console before it happens, so an interrupted swap is found
// and finished or rolled back from any computer. The console operations are injected: this
// module decides the order, the caller supplies the PS5.

export interface SwapDeps {
  exists(path: string): Promise<boolean>;
  mkdir(path: string): Promise<void>;
  move(from: string, to: string): Promise<void>;
  writeText(path: string, text: string): Promise<void>;
  /** null when the file is not there. */
  readText(path: string): Promise<string | null>;
  remove(path: string): Promise<void>;
  /** Names in `dir` ([] when it is missing). */
  list(dir: string): Promise<string[]>;
  isRunning(titleId: string): Promise<boolean>;
  isRegistered(titleId: string): Promise<boolean>;
  unregister(titleId: string): Promise<void>;
  install(packagePath: string): Promise<{ ok: boolean; message?: string }>;
  sleep(ms: number): Promise<void>;
  now(): number;
}

export interface SwapInput {
  titleId: string;
  /** The dump's path on the console: a game folder or image. */
  dump: string;
  /** The package built from it, on the machine running the engine. */
  packagePath: string;
}

export type SwapStep = "park" | "release" | "install";

export interface SwapJournal extends SwapInput {
  v: 1;
  parked: string;
  /** What has happened: "parking" (about to move), "parked", "installing", "installed". */
  step: "parking" | "parked" | "installing" | "installed";
  at: number;
}

export interface SwapResult {
  ok: boolean;
  message?: string;
  /** The dump was moved back after a failure. */
  rolledBack?: boolean;
  journal?: SwapJournal;
}

const JOURNAL_DIR = "/data/ps5upload/swap";
const MANUAL_LIST = "/data/shadowmount/manual.lst";
/** SMP rescans every 15 s and let go of a parked image in ~12 s on the Phat. */
const RELEASE_POLL_MS = 3_000;
const RELEASE_TIMEOUT_MS = 90_000;
const SMP_LINKS = ["mount.lnk", "mount_img.lnk"];

export function journalPathFor(titleId: string): string {
  return `${JOURNAL_DIR}/${titleId}.json`;
}

/** Where a dump waits during the swap: `<its volume>/ps5upload/parked/<name>`, which no SMP
 *  scan root covers. Null for a path whose volume can't be told (or an installed game). */
export function parkedPathFor(dump: string): string | null {
  const p = dump.replace(/\/+$/, "");
  const name = p.slice(p.lastIndexOf("/") + 1);
  let volume: string | null = null;
  if (p.startsWith("/data/")) volume = "/data";
  else {
    const m = /^(\/mnt\/[^/]+)\/./.exec(p);
    if (m) volume = m[1];
  }
  if (!volume || !name) return null;
  return `${volume}/ps5upload/parked/${name}`;
}

async function writeJournal(j: SwapJournal, deps: SwapDeps) {
  await deps.writeText(journalPathFor(j.titleId), JSON.stringify(j));
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Park the dump, wait for SMP to let go, clear the registration, install. On a failure after
 *  the dump moved, it is moved back. */
export async function runSwap(
  input: SwapInput,
  deps: SwapDeps,
  onStep: (s: SwapStep) => void,
): Promise<SwapResult> {
  const parked = parkedPathFor(input.dump);
  if (!parked) {
    return { ok: false, message: `Can't tell which drive ${input.dump} is on, so it can't be parked safely.` };
  }
  if (await deps.isRunning(input.titleId)) {
    return { ok: false, message: "The game is running. Close the game on the PS5, then start again." };
  }
  if (await deps.exists(parked)) {
    return {
      ok: false,
      message: `${parked} already exists (an earlier swap kept it). Delete or move it first.`,
    };
  }
  if (!(await deps.exists(input.dump))) {
    return { ok: false, message: `${input.dump} is not on the console any more.` };
  }

  const journal: SwapJournal = { v: 1, ...input, parked, step: "parking", at: deps.now() };
  onStep("park");
  try {
    await deps.mkdir(JOURNAL_DIR);
    await writeJournal(journal, deps);
    await deps.mkdir(parked.slice(0, parked.lastIndexOf("/")));
    await deps.move(input.dump, parked);
  } catch (e) {
    // Nothing moved (or the move itself failed): drop the journal, report.
    await deps.remove(journalPathFor(input.titleId)).catch(() => {});
    return { ok: false, message: `Couldn't park the dump: ${errorText(e)}` };
  }
  journal.step = "parked";
  await writeJournal(journal, deps).catch(() => {});

  const undo = async (why: string): Promise<SwapResult> => {
    try {
      await rollbackSwap(journal, deps);
      return { ok: false, rolledBack: true, message: `${why} The dump is back where it was.` };
    } catch (e) {
      return {
        ok: false,
        message: `${why} Moving the dump back failed too (${errorText(e)}); it is at ${parked}. Convert lists this swap so you can roll it back.`,
      };
    }
  };

  onStep("release");
  const deadline = deps.now() + RELEASE_TIMEOUT_MS;
  for (;;) {
    // A listing that failed is "not yet", never "SMP let go": keep looking until the deadline.
    const names = await deps.list(`/user/app/${input.titleId}`).catch(() => null);
    if (names && !names.some((n) => SMP_LINKS.includes(n))) break;
    if (deps.now() >= deadline) {
      return undo("ShadowMountPlus did not let go of the dump.");
    }
    await deps.sleep(RELEASE_POLL_MS);
  }
  try {
    if (await deps.isRegistered(input.titleId)) await deps.unregister(input.titleId);
  } catch (e) {
    return undo(`Couldn't remove the dump's registration: ${errorText(e)}.`);
  }

  onStep("install");
  journal.step = "installing";
  await writeJournal(journal, deps).catch(() => {});
  let result: { ok: boolean; message?: string };
  try {
    result = await deps.install(input.packagePath);
  } catch (e) {
    result = { ok: false, message: errorText(e) };
  }
  if (!result.ok) {
    return undo(`The install did not finish${result.message ? ` (${result.message})` : ""}.`);
  }
  journal.step = "installed";
  await writeJournal(journal, deps).catch(() => {});
  return { ok: true, journal };
}

/** Move a parked dump back where it was and forget the swap. SMP re-registers it. */
export async function rollbackSwap(journal: SwapJournal, deps: SwapDeps): Promise<void> {
  if (await deps.exists(journal.parked)) {
    if (await deps.exists(journal.dump)) {
      throw new Error(`${journal.dump} exists again, so the parked dump can't go back over it`);
    }
    await deps.move(journal.parked, journal.dump);
  }
  await deps.remove(journalPathFor(journal.titleId));
}

/** After a successful install: delete the parked dump, or keep it parked. Either way SMP's
 *  manual list stops naming the old path, and the swap is forgotten. */
export async function finishSwap(
  journal: SwapJournal,
  choice: "delete" | "keep",
  deps: SwapDeps,
): Promise<void> {
  if (choice === "delete") await deps.remove(journal.parked);
  // Each step may throw: the swap is forgotten only once all of them are done.
  const list = await deps.readText(MANUAL_LIST);
  if (list !== null) {
    const next = removeManualListLine(list, journal.dump);
    if (next !== null) await deps.writeText(MANUAL_LIST, next);
  }
  await deps.remove(journalPathFor(journal.titleId));
}

/** `existing` without the line naming `path`, or null when there is none. */
export function removeManualListLine(existing: string, path: string): string | null {
  const lines = existing.split(/\r?\n/);
  const kept = lines.filter((ln) => ln.trim() !== path.trim());
  if (kept.length === lines.length) return null;
  const body = kept.join("\n").replace(/\s+$/, "");
  return body ? `${body}\n` : "";
}

/** Swaps left unfinished on the console. */
export async function readJournals(deps: SwapDeps): Promise<SwapJournal[]> {
  const names = await deps.list(JOURNAL_DIR).catch(() => []);
  const out: SwapJournal[] = [];
  for (const n of names) {
    if (!n.endsWith(".json")) continue;
    const text = await deps.readText(`${JOURNAL_DIR}/${n}`).catch(() => null);
    if (!text) continue;
    try {
      const j = JSON.parse(text) as SwapJournal;
      if (j.v === 1 && j.titleId && j.dump && j.parked) out.push(j);
    } catch {
      /* a damaged journal is left for a person to look at */
    }
  }
  return out;
}
