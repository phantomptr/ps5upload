import { useState } from "react";
import { Check, Square, SquareCheck } from "lucide-react";
import { ApiError, call } from "../api";
import { t } from "../strings";
import { Modal } from "../ui/Modal";
import { filesErrorText } from "./Files";
import { watchOp } from "./opJob";
import { hasPerm, keepsPermissions, modeLetters, modeText, PERM_BITS, PERM_WHO, togglePerm, type PermBit, type PermWho } from "./permissions";

const WHO_KEY: Record<PermWho, string> = { owner: "cw_files_perm_owner", group: "cw_files_perm_group", other: "cw_files_perm_other" };
const BIT_KEY: Record<PermBit, string> = { read: "cw_files_perm_read", write: "cw_files_perm_write", exec: "cw_files_perm_exec" };

/** Apply `mode` to `path` (and below it with `recursive`): /api/fs/chmod answers {} at once, or a job for a folder's contents. */
/** A failed Apply in words: a job the console lost, the change refused ("fs_chmod_failed: …"), or why not. */
export function permErrorText(e: unknown): string | null {
  if (e === "gone") return t("cw_files_op_gone");
  if (e instanceof ApiError && /chmod/.test(e.code)) return t("cw_files_perm_failed");
  return filesErrorText(e);
}

export async function applyMode(path: string, mode: number, recursive: boolean): Promise<void> {
  const r = await call<{ job?: string }>("/api/fs/chmod", { path, mode: modeText(mode), recursive });
  if (r && typeof r.job === "string") {
    const end = await watchOp(r.job);
    // gone: the console no longer has the job (said as such); failed: its first word (no paths)
    if (end.end !== "done") throw end.end === "failed" ? new ApiError(500, (end.status.message ?? "failed").split(":")[0]) : end.end;
  }
}

/**
 * Info → Permissions: owner / group / other × read / write / run toggles (one
 * button each, ✕ flips it) with the octal value, "Apply to everything inside"
 * for a folder, Apply / Cancel. On an exFAT or FAT drive it says permissions
 * are not kept there.
 */
export function PermissionsDialog({
  name,
  path,
  mode: initial,
  dir,
  fsType,
  onDone,
  onCancel,
}: {
  name: string;
  path: string;
  mode: number;
  dir: boolean;
  fsType?: string | null;
  /** Applied: the new mode. */
  onDone: (mode: number) => void;
  onCancel: () => void;
}) {
  const [mode, setMode] = useState(initial & 0o777);
  const [recursive, setRecursive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const apply = () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    applyMode(path, mode, dir && recursive).then(
      () => onDone(mode),
      (e: unknown) => {
        setBusy(false);
        setError(permErrorText(e));
      },
    );
  };
  return (
    <Modal label={t("cw_files_perm_title", { name })} className="cw-perm" onBack={onCancel}>
      <h2 className="cw-file-name">{t("cw_files_perm_title", { name })}</h2>
      {!keepsPermissions(fsType) ? (
        <p className="cw-msg is-info" data-perm-note="">
          {t("cw_files_perm_not_kept", { fs: fsType ?? "" })}
        </p>
      ) : null}
      <table className="cw-perm-grid">
        <thead>
          <tr>
            <th />
            {PERM_BITS.map((b) => (
              <th key={b} scope="col">
                {t(BIT_KEY[b])}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {PERM_WHO.map((who, wi) => (
            <tr key={who}>
              <th scope="row">{t(WHO_KEY[who])}</th>
              {PERM_BITS.map((bit, bi) => {
                const on = hasPerm(mode, who, bit);
                return (
                  <td key={bit}>
                    <button
                      type="button"
                      className="cw-focusable cw-button cw-perm-bit"
                      data-perm={`${who}-${bit}`}
                      data-nav-default={wi === 0 && bi === 0 ? "" : undefined}
                      aria-pressed={on}
                      aria-label={`${t(WHO_KEY[who])} · ${t(BIT_KEY[bit])}`}
                      disabled={busy}
                      onClick={() => setMode((m) => togglePerm(m, who, bit))}
                    >
                      {on ? <Check aria-hidden /> : <span aria-hidden>–</span>}
                    </button>
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
      <p className="cw-perm-value">
        <span className="cw-dim">{t("cw_files_perm_value")}</span> <strong className="cw-mono" data-perm-value="">{modeText(mode)}</strong>{" "}
        <span className="cw-dim cw-mono">{modeLetters(mode)}</span>
      </p>
      {dir ? (
        <button
          type="button"
          className="cw-focusable cw-button"
          data-action="perm-recursive"
          aria-pressed={recursive}
          disabled={busy}
          onClick={() => setRecursive((v) => !v)}
        >
          {recursive ? <SquareCheck aria-hidden /> : <Square aria-hidden />}
          <span>{t("cw_files_perm_recursive")}</span>
        </button>
      ) : null}
      {busy ? (
        <p className="cw-dim" role="status">
          {t("cw_files_perm_applying")}
        </p>
      ) : null}
      {error ? (
        <p className="cw-field-error" role="alert">
          {error}
        </p>
      ) : null}
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" data-action="perm-cancel" onClick={onCancel}>
          <span>{t("cw_dialog_cancel")}</span>
        </button>
        <button type="button" className="cw-focusable cw-button is-primary" data-action="perm-apply" disabled={busy} onClick={apply}>
          <span>{t("cw_files_perm_apply")}</span>
        </button>
      </div>
    </Modal>
  );
}
