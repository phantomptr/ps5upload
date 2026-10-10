import { useState, type Dispatch, type SetStateAction } from "react";

import { fsDelete, type ScreenshotEntry } from "../../api/ps5";
import { useConfirm } from "../../components/ConfirmDialog";
import { consoleAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { audit } from "../../state/auditLog";
import { useTr } from "../../state/lang";
import { withConsolePrefix } from "../../state/roster";
import { deleteCaptures } from "./deleteCaptures";

/**
 * "Delete N" for the Screenshots and Video clips lists: confirm, delete the
 * selection with the same engine delete Files uses, drop what's gone from the
 * list without a re-read, and keep only the failures selected.
 */
export function useCaptureDelete({
  host,
  kind,
  selected,
  setSelected,
  setItems,
  setError,
}: {
  host: string | null | undefined;
  kind: "screenshots" | "videos";
  selected: Set<string>;
  setSelected: (s: Set<string>) => void;
  setItems: Dispatch<SetStateAction<ScreenshotEntry[] | null>>;
  setError: (e: string | null) => void;
}) {
  const tr = useTr();
  const [deleting, setDeleting] = useState(false);
  const { confirm, dialog } = useConfirm();

  async function deleteSelected() {
    const target = host?.trim();
    if (!target || selected.size === 0) return;
    const n = selected.size;
    const ok = await confirm({
      title:
        kind === "screenshots"
          ? tr("screenshots_delete_confirm_title", { n }, `Delete ${n} screenshots?`)
          : tr("videos_delete_confirm_title", { n }, `Delete ${n} clips?`),
      message: tr(
        "captures_delete_confirm_body",
        undefined,
        "They are removed from the PS5's capture storage. This can't be undone.",
      ),
      confirmLabel: tr("delete", undefined, "Delete"),
      destructive: true,
    });
    if (!ok) return;
    setDeleting(true);
    setError(null);
    try {
      const r = await deleteCaptures([...selected], (p) =>
        fsDelete(consoleAddr(target), p),
      );
      for (const p of r.deleted) audit("fs_delete", withConsolePrefix(target, p));
      const gone = new Set(r.deleted);
      setItems((prev) => prev?.filter((i) => !gone.has(i.path)) ?? prev);
      setSelected(new Set(r.failed.map((f) => f.path)));
      if (r.failed.length > 0) {
        const reason = humanizePs5Error(r.failed[0].error);
        setError(
          tr(
            "captures_delete_partial",
            { ok: r.deleted.length, failed: r.failed.length, reason },
            `Deleted ${r.deleted.length}; ${r.failed.length} failed (${reason}). The failed ones stay selected.`,
          ),
        );
      }
    } finally {
      setDeleting(false);
    }
  }

  return { deleting, deleteSelected, dialog };
}
