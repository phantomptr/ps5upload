import { useState } from "react";
import { Square, SquareCheck } from "lucide-react";
import { t } from "../strings";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Modal } from "../ui/Modal";
import { baseName } from "./move";
import type { Conflict, Decision } from "./paste";

/**
 * A paste met an item that is already there: Skip (the safe choice, focused
 * first; ○ too), Keep both, or Replace, which asks once more as a danger
 * confirm (the existing item is deleted). "Apply to all" answers the rest of
 * this paste's conflicts the same way.
 */
export function ConflictDialog({
  conflict,
  onAnswer,
  asking = false,
}: {
  conflict: Conflict;
  onAnswer: (d: Decision) => void;
  /** Replace is being confirmed already (tests). */
  asking?: boolean;
}) {
  const [all, setAll] = useState(false);
  const [confirming, setConfirming] = useState(asking);
  const name = baseName(conflict.dest);
  const skip = () => onAnswer({ choice: "skip", all });
  return (
    <Modal label={t("cw_files_conflict_title", { name })} className="cw-conflict" onBack={skip}>
      <h2 className="cw-sheet-title">{t("cw_files_conflict_title", { name })}</h2>
      <p className="cw-dim cw-mono">{conflict.dest}</p>
      <p>{t(conflict.canReplace ? "cw_files_conflict_body" : "cw_files_conflict_body_self")}</p>
      {conflict.left > 0 ? (
        <button
          type="button"
          className="cw-focusable cw-button cw-conflict-all"
          data-action="conflict-all"
          aria-pressed={all}
          onClick={() => setAll((v) => !v)}
        >
          {all ? <SquareCheck aria-hidden /> : <Square aria-hidden />}
          <span>{t("cw_files_conflict_all", { n: conflict.left })}</span>
        </button>
      ) : null}
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" data-conflict="skip" data-nav-default onClick={skip}>
          <span>{t("cw_files_conflict_skip")}</span>
        </button>
        <button
          type="button"
          className="cw-focusable cw-button"
          data-conflict="keep"
          onClick={() => onAnswer({ choice: "keep", all })}
        >
          <span>{t("cw_files_conflict_keep")}</span>
        </button>
        {conflict.canReplace ? (
          <button
            type="button"
            className="cw-focusable cw-button is-danger"
            data-conflict="replace"
            onClick={() => setConfirming(true)}
          >
            <span>{t("cw_files_conflict_replace")}</span>
          </button>
        ) : null}
      </div>
      {/* Over the dialog, which stays mounted: ○ returns to Replace, and the dialog keeps its opener. */}
      {confirming ? (
        <ConfirmDialog
          title={t(all ? "cw_files_conflict_replace_all_title" : "cw_files_conflict_replace_title", { name })}
          body={t(
            all
              ? "cw_files_conflict_replace_all_body"
              : conflict.destDir
                ? "cw_files_conflict_replace_dir_body"
                : "cw_files_conflict_replace_file_body",
          )}
          confirmLabel={t("cw_files_conflict_replace")}
          danger
          onConfirm={() => onAnswer({ choice: "replace", all })}
          onCancel={() => setConfirming(false)}
        />
      ) : null}
    </Modal>
  );
}
