import { useEffect, useRef, useState } from "react";
import { Save, Undo2 } from "lucide-react";
import { ApiError } from "../../api";
import { filesErrorText } from "../Files";
import { t } from "../../strings";
import { encodeText, saveText, stampOf, type FileStamp } from "../transfer";

/** Why a save did not happen, as the editor says it. */
export function saveErrorText(e: unknown): string | null {
  if (e === "binary" || (e instanceof ApiError && e.code === "binary")) return t("cw_files_edit_binary");
  if (e === "too_large" || (e instanceof ApiError && e.code === "too_large")) return t("cw_files_edit_too_large");
  if (e instanceof ApiError && e.code === "gone") return t("cw_files_edit_gone");
  if (e instanceof ApiError && e.code === "no_space") return t("cw_files_edit_no_space");
  return filesErrorText(e);
}

/**
 * How a file breaks its lines: saved with its dominant ending (CRLF when it has more CRLF than lone LF
 * breaks), and whether it has lone CRs (old Mac style), which the editor turns into that ending: said
 * before the first save, never silently.
 */
export function lineEndings(text: string): { crlf: boolean; loneCr: boolean } {
  let crlf = 0;
  let lf = 0;
  let cr = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if (c === 13) {
      if (text.charCodeAt(i + 1) === 10) {
        crlf++;
        i++;
      } else cr++;
    } else if (c === 10) lf++;
  }
  return { crlf: crlf > lf, loneCr: cr > 0 };
}

/** The text as the box shows it: every break a "\n" (the box's own form). */
export function editorText(text: string): string {
  return text.replace(/\r\n?/g, "\n");
}

type Phase = { kind: "edit" } | { kind: "saving" } | { kind: "changed" } | { kind: "leaving" } | { kind: "error"; text: string };

/**
 * The text editor: one textarea with the whole file (the PS5 shows its
 * keyboard; a phone its own), Save and Cancel. Save writes the file in one
 * step on the console (a temporary file renamed onto it, its permissions
 * kept); when the file changed since it was opened it asks first (Save
 * anyway / Discard mine). Leaving with unsaved changes asks too.
 */
export function TextEditor({
  path,
  text,
  stamp,
  onSaved,
  onClose,
}: {
  path: string;
  /** The file as read (line endings as written). */
  text: string;
  stamp: FileStamp;
  onSaved: (stamp: FileStamp) => void;
  /** Leave the editor without saving (asked first when there are changes). */
  onClose: () => void;
}) {
  const { crlf, loneCr } = lineEndings(text);
  const original = editorText(text);
  const [value, setValue] = useState(original);
  const [phase, setPhase] = useState<Phase>({ kind: "edit" });
  const box = useRef<HTMLTextAreaElement>(null);
  const dirty = value !== original;
  const live = useRef(true);
  useEffect(
    () => () => {
      live.current = false;
    },
    [],
  );

  const save = (force: boolean) => {
    const bytes = encodeText(value, crlf);
    if (typeof bytes === "string") {
      setPhase({ kind: "error", text: saveErrorText(bytes) ?? "" });
      return;
    }
    setPhase({ kind: "saving" });
    (async () => {
      const at = force ? await stampOf(path) : stamp;
      return saveText(path, bytes, at);
    })().then(
      (next) => {
        if (live.current) onSaved(next);
      },
      (e: unknown) => {
        if (!live.current) return;
        if (e instanceof ApiError && e.code === "changed") setPhase({ kind: "changed" });
        else setPhase({ kind: "error", text: saveErrorText(e) ?? "" });
      },
    );
  };
  const leave = () => (dirty ? setPhase({ kind: "leaving" }) : onClose());

  return (
    <div className="cw-editor" data-editor="">
      <textarea
        ref={box}
        className="cw-focusable cw-input cw-editor-box"
        value={value}
        spellCheck={false}
        autoCapitalize="off"
        autoComplete="off"
        aria-label={t("cw_files_edit_label")}
        data-action="edit-box"
        data-nav-default=""
        readOnly={phase.kind === "saving"}
        onChange={(e) => setValue(e.target.value)}
      />
      {loneCr ? (
        <p className="cw-dim" data-edit-eol="">
          {t("cw_files_edit_eol_note")}
        </p>
      ) : null}
      {phase.kind === "changed" ? (
        <div className="cw-msg is-error" role="alert" data-edit-changed="">
          {t("cw_files_edit_changed")}
          <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="edit-force" onClick={() => save(true)}>
            <Save aria-hidden />
            <span>{t("cw_files_edit_save_anyway")}</span>
          </button>
          <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="edit-discard" onClick={onClose}>
            <Undo2 aria-hidden />
            <span>{t("cw_files_edit_discard")}</span>
          </button>
        </div>
      ) : null}
      {phase.kind === "leaving" ? (
        <div className="cw-msg" role="alert" data-edit-leaving="">
          {t("cw_files_edit_unsaved")}
          <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="edit-keep" onClick={() => setPhase({ kind: "edit" })}>
            <span>{t("cw_files_edit_keep")}</span>
          </button>
          <button type="button" className="cw-focusable cw-button cw-msg-action is-danger" data-action="edit-discard" onClick={onClose}>
            <Undo2 aria-hidden />
            <span>{t("cw_files_edit_discard")}</span>
          </button>
        </div>
      ) : null}
      {phase.kind === "error" && phase.text ? (
        <p className="cw-field-error" role="alert">
          {phase.text}
        </p>
      ) : null}
      {phase.kind === "saving" ? (
        <p className="cw-dim" role="status">
          {t("cw_files_edit_saving")}
        </p>
      ) : null}
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" data-action="edit-cancel" onClick={leave}>
          <span>{t("cw_dialog_cancel")}</span>
        </button>
        <button
          type="button"
          className="cw-focusable cw-button is-primary"
          data-action="edit-save"
          disabled={!dirty || phase.kind === "saving"}
          onClick={() => save(false)}
        >
          <Save aria-hidden />
          <span>{t("cw_files_edit_save")}</span>
        </button>
      </div>
    </div>
  );
}

/** Leaving the editor (○ / Back): the editor's own Cancel, so unsaved changes are asked about. */
export const EDITOR_CANCEL = '[data-editor] [data-action="edit-cancel"]';
