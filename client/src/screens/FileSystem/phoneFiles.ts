// Files on a phone (#932, #936): what system Back does on the listing, and what the compact header
// shows. Kept apart from the screen so it can be tested on its own.

/** What one press of system Back does on the Files listing once no menu, sheet or dialog is open
 *  (those close first, lib/systemBack). Null: nothing here, the press leaves Files. */
export type FilesBackStep = "end-selection" | "cancel-staged" | "up" | null;

/**
 * Leave selection mode first, then go up one folder, and leave Files from the top. What is staged to
 * copy or move stays while Back goes up (that is how one gets to the folder to paste into); at the
 * top, Back lets go of it first (its bar is where the bottom navigation was), then leaves Files.
 *
 * The top is the drive's root (/data, /mnt/usb0): a phone's file manager stops at its storage, and
 * the folders above a drive (/mnt) are not where anyone was. Outside every known drive it is "/".
 * A view over the listing (search, space, snapshots) is its own history entry: it takes Back by
 * itself, so there is no step while one is open.
 */
export function filesBackStep(s: {
  /** A view covers the listing. */
  view: boolean;
  /** Selection mode: something is selected, or "Select" was picked. */
  selecting: boolean;
  /** Something is staged to copy or move, and its bar shows (#936). */
  staged?: boolean;
  path: string;
  /** The drive the path is on, or null outside every known drive. */
  driveRoot: string | null;
}): FilesBackStep {
  if (s.view) return null;
  if (s.selecting) return "end-selection";
  if (backUpTarget(s.path, s.driveRoot) !== null) return "up";
  return s.staged ? "cancel-staged" : null;
}

/** Where Back goes up to from `path`, or null at the top (see filesBackStep). */
export function backUpTarget(path: string, driveRoot: string | null): string | null {
  const here = trimSlash(path);
  if (here === "/" || here === "") return null;
  if (driveRoot !== null && here === trimSlash(driveRoot)) return null;
  const cut = here.lastIndexOf("/");
  return cut <= 0 ? "/" : here.slice(0, cut);
}

/** The open folder's own name, for the compact header; null at "/" (the header says Files). */
export function folderTitle(path: string): string | null {
  const here = trimSlash(path);
  if (here === "/" || here === "") return null;
  return here.slice(here.lastIndexOf("/") + 1);
}

/** A name or path field just opened on a phone (#936): bring it into view once the on-screen
 *  keyboard is up, so the keyboard does not cover what is typed. The keyboard shrinks the visual
 *  viewport a moment after the focus, so it is shown again when that happens. */
export function revealAboveKeyboard(el: HTMLElement | null, phone: boolean): void {
  if (!el || !phone || typeof window === "undefined") return;
  const show = () => el.scrollIntoView?.({ block: "nearest" });
  show();
  const vv = window.visualViewport;
  if (!vv) return;
  vv.addEventListener("resize", show);
  window.setTimeout(() => vv.removeEventListener("resize", show), 1000);
}

function trimSlash(p: string): string {
  return p.length > 1 && p.endsWith("/") ? p.replace(/\/+$/, "") || "/" : p;
}
