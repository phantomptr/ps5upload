import type { InstallResult } from "../../state/consoleQueueBridge";
import { pkgLibraryStore, type PkgEntry } from "../../state/pkgLibrary";

export type StagedPkgAction = "install" | "reinstall" | "busy";

/** What the row's button does: a package already waiting or running has no
 *  button (its progress is on the queue), one this app installed reinstalls. */
export function stagedPkgAction(entry: Pick<PkgEntry, "status" | "installedHere">): StagedPkgAction {
  if (entry.status !== "idle") return "busy";
  return entry.installedHere ? "reinstall" : "install";
}

/**
 * Queue the install of an update/DLC already staged on the console: the same
 * call Install Package makes for its own rows (the package library's
 * `install`, which goes through the console queue), so progress, retries and
 * the result look the same wherever it was started.
 *
 * `confirmWithoutBase` is asked first when the console doesn't have the base
 * game: Sony's installer accepts an update or DLC then and applies nothing.
 * Returns null when the user declined.
 */
export async function installStagedPkg(opts: {
  host: string;
  entry: Pick<PkgEntry, "path">;
  baseInstalled: boolean;
  confirmWithoutBase: () => Promise<boolean>;
}): Promise<InstallResult | null> {
  if (!opts.baseInstalled && !(await opts.confirmWithoutBase())) return null;
  return pkgLibraryStore(opts.host).getState().install(opts.entry.path, opts.host);
}
