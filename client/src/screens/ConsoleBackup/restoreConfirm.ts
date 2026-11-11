import type { ConfirmOptions } from "../../components/ConfirmDialog";
import type { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

/** The question asked before a console backup is put back: it replaces files on the PS5, as the
 *  snapshot restore (Backup) already asks. */
export function restoreConfirmOptions(tr: Tr): ConfirmOptions {
  return {
    title: tr("cb_restore_start", undefined, "Restore to this PS5"),
    message: tr(
      "cb_restore_hint",
      undefined,
      "Files with the same name on the PS5 are replaced. Nothing else there is deleted. Saves are backed up first.",
    ),
    confirmLabel: tr("backup_restore", undefined, "Restore"),
    destructive: true,
  };
}

/** Run the restore only once the user said yes. Returns whether it ran. */
export async function restoreAfterConfirm(
  confirm: (o: ConfirmOptions) => Promise<boolean>,
  opts: ConfirmOptions,
  run: () => Promise<unknown>,
): Promise<boolean> {
  if (!(await confirm(opts))) return false;
  await run();
  return true;
}
