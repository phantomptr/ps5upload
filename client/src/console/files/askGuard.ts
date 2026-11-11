// Files' dialogs act on bare names in "the folder shown". These keep a dialog, a late answer
// and a finished extract from acting on a folder (or a dialog) other than the one they began in.

/**
 * A late answer (Load, Eject, Unmount, Remove share) belongs to the dialog that asked. When
 * that dialog was closed and another opened meanwhile, the answer must not close the new one
 * or write its error into it. Dialogs are compared by identity: every opening is a new object.
 */
export function stillAsking<T extends object>(now: T | null, asked: T | null): boolean {
  return asked != null && now === asked;
}

/**
 * Whether a finished extract opens the folder it extracted into. Only when the user is still
 * in the folder the extract began in and no dialog is open: opening a folder closes the
 * dialog (its names belong to the folder left), and takes the user from where they went.
 */
export function extractOpensDest(into: string, began: string, now: string | null, dialogOpen: boolean): boolean {
  return into !== began && now === began && !dialogOpen;
}

/**
 * "Delete the archive after extracting": only when every entry came out. A file that was
 * skipped because one was already there exists nowhere else in its archived form.
 */
export function extractLeftSomething(r: { skipped: number; unsafe: number; links: number }): boolean {
  return r.skipped > 0 || r.unsafe > 0 || r.links > 0;
}
