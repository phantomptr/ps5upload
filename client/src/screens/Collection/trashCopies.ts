// Move to Trash: preview (the engine checks every path is a copy it knows), confirm, apply.
// The engine rescans afterwards; reloading follows that scan.

import { collection } from "../../api/collection";
import type { ConfirmOptions } from "../../components/ConfirmDialog";
import { formatCollectionBytes } from "../../lib/collectionView";
import { loadCollection } from "../../state/collection";
import type { Translator } from "../../state/lang";
import { pushNotification } from "../../state/notifications";

export async function trashCopies(
  paths: string[],
  confirm: (o: ConfirmOptions) => Promise<boolean>,
  tr: Translator,
): Promise<void> {
  let preview;
  try {
    preview = await collection.trashPreview(paths);
  } catch (e) {
    pushNotification("error", e instanceof Error ? e.message : String(e));
    return;
  }
  if (!preview.available) {
    pushNotification(
      "error",
      tr(
        "collection.trash_unavailable_v2",
        undefined,
        "This engine has no Trash. To delete copies from here, turn on “Allow deleting copies for good” in Collection → Folders.",
      ),
    );
    return;
  }
  const n = preview.items.length;
  const forGood = preview.mode === "delete";
  const ok = forGood
    ? await confirm({
        title:
          n === 1
            ? tr(
                "collection.delete_one_title",
                { name: preview.items[0].name },
                "Delete “{name}” for good?",
              )
            : tr(
                "collection.delete_n_title",
                { n },
                "Delete {n} copies for good?",
              ),
        message: tr(
          "collection.delete_body",
          { size: formatCollectionBytes(preview.total_bytes) },
          "{size} in total. This engine has no Trash: the files are deleted and cannot be brought back.",
        ),
        confirmLabel: tr("collection.delete_go", undefined, "Delete for good"),
        destructive: true,
      })
    : await confirm({
        title:
          n === 1
            ? tr(
                "collection.trash_one_title",
                { name: preview.items[0].name },
                "Move “{name}” to the Trash?",
              )
            : tr(
                "collection.trash_n_title",
                { n },
                "Move {n} copies to the Trash?",
              ),
        message: tr(
          "collection.trash_body",
          { size: formatCollectionBytes(preview.total_bytes) },
          "{size} in total. They can be put back from the Trash until it is emptied.",
        ),
        confirmLabel: tr("collection.trash_go", undefined, "Move to Trash"),
        destructive: true,
      });
  if (!ok) return;
  try {
    const r = await collection.trashApply(preview.token);
    if (r.failed.length > 0) {
      pushNotification(
        "error",
        tr(
          "collection.trash_failed",
          { n: r.failed.length },
          "{n} could not be moved to the Trash",
        ),
        { body: r.failed.join("\n") },
      );
    }
    if (r.moved.length > 0) {
      pushNotification(
        "info",
        forGood
          ? tr(
              "collection.delete_done",
              { n: r.moved.length },
              "Deleted {n} for good",
            )
          : tr(
              "collection.trash_done",
              { n: r.moved.length },
              "Moved {n} to the Trash",
            ),
      );
    }
  } catch (e) {
    pushNotification("error", e instanceof Error ? e.message : String(e));
  }
  await loadCollection();
}
