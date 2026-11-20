// A game the PS5's own installer has installed (#884).
//
// Adding a game folder to the home screen stages /user/app/<id> on the console's INTERNAL drive
// and asks the PS5's installer to install what is staged there. For a title the console already
// has installed from a package, on M.2 or extended storage, that rewrote the console's record of
// the game: Among Us on M.2, dumped to a USB drive, then Play on the dump, and the PS5 listed the
// game under Console Storage, where it has no package, and could not start it any more.
//
// The helper (12.6.16 on) refuses such a register before writing anything. This is the same
// check on the app's side, so an older helper is protected too, and the words for the refusal.

import { trStatic } from "./trStatic";

/** Where the console's install is: `record` = only its records name a package location (that
 *  storage may be unplugged). The words the helper's refusals carry. */
export type InstallWhere = "internal" | "m2" | "usb" | "extended" | "record";

/** The storages a package install can live on, as the helper looks at them: internal, M.2, USB
 *  extended storage. */
export const INSTALL_VOLUMES: ReadonlyArray<readonly [string, InstallWhere]> = [
  ["", "internal"],
  ["/mnt/ext1", "m2"],
  ["/mnt/ext0", "usb"],
];

const TITLE_ID = /^[A-Za-z]{4}\d{5}$/;

export interface ConsoleInstall {
  where: InstallWhere;
  /** The package found, e.g. /mnt/ext1/user/app/PPSA03595/app.pkg. */
  pkg: string;
}

/** `exists` answers whether a console path is there; it throws when the console can't say. */
export interface InstallProbe {
  exists(path: string): Promise<boolean>;
}

/** The console's own install of `titleId`, or null. Rejects when the console could not answer:
 *  "can't tell" must never read as "not installed". */
export async function findConsoleInstall(
  probe: InstallProbe,
  titleId: string,
): Promise<ConsoleInstall | null> {
  if (!TITLE_ID.test(titleId)) return null;
  const id = titleId.toUpperCase();
  for (const [volume, where] of INSTALL_VOLUMES) {
    const pkg = `${volume}/user/app/${id}/app.pkg`;
    if (await probe.exists(pkg)) return { where, pkg };
  }
  return null;
}

/** A helper refusal (or the app's own) for a title the console installed: what was refused and
 *  where the install is. */
export function installRefusal(
  raw: string,
): { op: "register" | "unregister"; where: InstallWhere } | null {
  const m = /\b(register|unregister)_title_installed(?::(internal|m2|usb|extended|record))?\b/.exec(raw);
  if (!m) return null;
  return { op: m[1] as "register" | "unregister", where: (m[2] as InstallWhere | undefined) ?? "record" };
}

export function installWhereLabel(where: InstallWhere): string {
  switch (where) {
    case "internal":
      return trStatic("console_install_where_internal", "console storage");
    case "m2":
      return trStatic("console_install_where_m2", "M.2 storage");
    case "usb":
      return trStatic("console_install_where_usb", "USB extended storage");
    case "extended":
      return trStatic("console_install_where_extended", "extended storage");
    default:
      return trStatic("console_install_where_record", "a storage drive that isn't connected right now");
  }
}

/** The sentence for a refused register: the installed game stays as it is. */
export function registerRefusedText(where: InstallWhere, name?: string): string {
  return trStatic(
    "console_install_register_refused",
    "{name} is already installed on {where}. Start it from the home screen. A copy with the same title can't be added to the home screen while the installed game is there, so nothing was changed.",
    { name: name || trStatic("console_install_this_game", "This game"), where: installWhereLabel(where) },
  );
}

/** The sentence for a refused "remove from home screen" of a folder or image row. */
export function unregisterRefusedText(where: InstallWhere, name?: string): string {
  return trStatic(
    "console_install_unregister_refused",
    "{name} is installed on {where} by the PS5 itself, so it was left alone: removing this copy from the home screen would have uninstalled that game. To remove the installed game, use Uninstall on its page or Settings › Storage on the PS5.",
    { name: name || trStatic("console_install_this_game", "This game"), where: installWhereLabel(where) },
  );
}

/** The title id in a game folder's sce_sys/param.json text, or null. */
export function titleIdFromParamJson(text: string): string | null {
  try {
    const v = JSON.parse(text) as { titleId?: unknown };
    return typeof v.titleId === "string" && TITLE_ID.test(v.titleId) ? v.titleId.toUpperCase() : null;
  } catch {
    return null;
  }
}

/** The refusal the app raises itself (same text as the helper's, so one humanizer reads both). */
export class ConsoleInstallRefusal extends Error {
  constructor(
    readonly op: "register" | "unregister",
    readonly install: ConsoleInstall,
  ) {
    super(`${op}_title_installed:${install.where} (${install.pkg})`);
    this.name = "ConsoleInstallRefusal";
  }
}

/** What an earlier PS5Upload left when it added a copy of a game the console had installed on
 *  M.2 or extended storage (#884): the package is still there, but /user/app/<id> on the internal
 *  drive holds PS5Upload's staged files (sce_sys plus a top-level icon0.png or mount.lnk) and no
 *  package, so the PS5 lists the game under Console Storage and cannot start it. */
export interface DisplacedInstall {
  titleId: string;
  where: InstallWhere;
  pkg: string;
  /** PS5Upload's files found in /user/app/<id> on the internal drive. */
  stagedFiles: string[];
}

export async function findDisplacedInstall(
  probe: InstallProbe,
  titleId: string,
): Promise<DisplacedInstall | null> {
  if (!TITLE_ID.test(titleId)) return null;
  const id = titleId.toUpperCase();
  const internal = `/user/app/${id}`;
  // A package on internal storage is a normal internal install.
  if (await probe.exists(`${internal}/app.pkg`)) return null;
  let found: ConsoleInstall | null = null;
  for (const [volume, where] of INSTALL_VOLUMES) {
    if (!volume) continue;
    const pkg = `${volume}/user/app/${id}/app.pkg`;
    if (await probe.exists(pkg)) {
      found = { where, pkg };
      break;
    }
  }
  if (!found) return null;
  if (!(await probe.exists(`${internal}/sce_sys`))) return null;
  const staged = [`${internal}/sce_sys`];
  for (const f of ["icon0.png", "mount.lnk", "mount_img.lnk"]) {
    if (await probe.exists(`${internal}/${f}`)) staged.push(`${internal}/${f}`);
  }
  // sce_sys alone is not PS5Upload's signature: its register also leaves a top-level icon0.png
  // (or a mount.lnk) next to it.
  if (staged.length < 2) return null;
  return { titleId: id, where: found.where, pkg: found.pkg, stagedFiles: staged };
}
