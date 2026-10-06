// What to do when the PS5 refuses to start a non-Sony title (0x80940033).
//
// Sony's launcher returns 0x80940033 for every homebrew or fake-package title when the
// console's fake-package support is not loaded: on FW 13.60 that is kstuff. It is not a
// problem with the one game, so "re-register" or "fake packages above 11.60" send people
// the wrong way. Two causes are fixable from here:
//   - kstuff is not running: say so, since nothing else will start until it is loaded;
//   - kstuff is running but the game's folder came from ps5upload 6.0-6.1.2, which kept
//     the computer's file modes (0644 from Windows). The loader wants world-execute, so
//     the folder is made 0777 and the launch is tried once more.

import { runningChainPayloads } from "./runningChain";

/** The launcher's "this console will not start non-Sony titles" refusal. */
export function isHomebrewRefusal(raw: string): boolean {
  return /launch_sony_error_0x80940033/i.test(raw);
}

export interface RefusalDeps {
  /** The console's process list (names and comms). */
  processes: () => Promise<ReadonlyArray<{ name: string; comm?: string }>>;
  /** chmod -R 0777 on the title's folder. */
  chmod777: (path: string) => Promise<void>;
  /** Launch the title again. */
  relaunch: () => Promise<void>;
}

export type RefusalOutcome =
  /** kstuff is not running: the console starts no homebrew or fake-package title. */
  | { kind: "no_kstuff" }
  /** The folder was made 0777 and the second launch was accepted. */
  | { kind: "fixed" }
  /** Nothing here could fix it (no folder to repair, or it was refused again). */
  | { kind: "refused" };

/** Works out why the launch was refused and repairs what it can. Never throws: a
 *  process list or chmod that fails just leaves the refusal as it was. */
export async function handleHomebrewRefusal(
  source: string | null | undefined,
  deps: RefusalDeps,
): Promise<RefusalOutcome> {
  let kstuff: boolean | null;
  try {
    kstuff = runningChainPayloads(await deps.processes()).has("kstuff");
  } catch {
    kstuff = null; // unknown: still try the folder repair
  }
  if (kstuff === false) return { kind: "no_kstuff" };
  if (!source) return { kind: "refused" };
  try {
    await deps.chmod777(source);
    await deps.relaunch();
    return { kind: "fixed" };
  } catch {
    return { kind: "refused" };
  }
}
