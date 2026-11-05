import { useEffect, useState } from "react";
import { formatBytes } from "../../lib/format";
import { archiveFolderName } from "../../lib/archiveName";
import { ApiError } from "../api";
import { t } from "../strings";
import { topModal } from "../nav";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Modal } from "../ui/Modal";
import { refocusIfLost } from "../ui/restoreFocus";
import { filesErrorText } from "./Files";
import { joinPath } from "./move";
import { freeName, statOrNull } from "./paste";
import { extractInfo, type ExtractInfo } from "./opJob";

type Load = { info: ExtractInfo } | { error: string | null; generic: boolean } | null;

/** How an extract is started (the password is asked for later, only when the archive has one). */
export interface ExtractStart {
  dest: string;
  merge: boolean;
  replace: boolean;
  /** Delete the archive once every entry came out. */
  deleteAfter?: boolean;
}

/** An extract cause token (fs_extract_no_space, …) as a sentence, or null when it has none. */
export function extractCauseText(code: string): string | null {
  const c = code.replace(/^fs_extract_/, "");
  switch (c) {
    case "no_space":
      return t("cw_files_extract_err_no_space");
    case "destination_exists":
      return t("cw_files_err_exists");
    case "busy":
      return t("cw_files_extract_err_busy");
    case "password_required":
      return t("cw_files_extract_err_password");
    case "wrong_password":
      return t("cw_files_extract_pw_wrong");
    case "rar_encrypted":
      return t("cw_files_extract_err_rar_encrypted");
    case "unsupported_method":
      return t("cw_files_extract_err_method");
    case "corrupt":
    case "truncated":
      return t("cw_files_extract_err_corrupt");
    case "not_archive":
      return t("cw_files_extract_err_not_archive");
    case "missing_volume":
      return t("cw_files_extract_err_missing_volume");
    case "loader_unreachable":
      return t("cw_files_extract_err_loader");
    case "not_started":
    case "died":
      return t("cw_files_extract_err_died");
    case "no_memory":
      return t("cw_files_extract_err_no_memory");
    case "write_failed":
      return t("cw_files_extract_err_write");
    case "open_failed":
      return t("cw_files_err_read");
    case "path_not_allowed":
    case "bad_dest":
    case "missing_path":
      return t("cw_files_err_denied");
  }
  return null;
}

/** Why /api/fs/extract/info or /api/fs/extract refused, as a sentence (null: the reload banner says it). */
export function extractErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    const why = extractCauseText(err.code.replace(/[:\s].*$/, ""));
    if (why) return why;
  }
  return filesErrorText(err);
}

/**
 * Extract an archive on the console into a folder next to it (named after
 * it; the archive stays). Before it starts: where it writes, the archive's
 * size and parts, the free space there. A folder already there offers Keep
 * both (a new folder, "name (2)"), Merge (files already there are skipped)
 * and Merge and replace (they are overwritten: confirmed first, as a danger).
 * Start hands it to Files, which runs it as the page's one batch.
 */
export function ExtractDialog({
  dir,
  name,
  path,
  busy,
  onStart,
  onCancel,
  load = extractInfo,
  taken = async (p: string) => (await statOrNull(p)) != null,
  initial = null,
  confirming = false,
}: {
  dir: string;
  name: string;
  path: string;
  /** Another batch runs: Start waits. */
  busy: boolean;
  onStart: (start: ExtractStart) => void;
  onCancel: () => void;
  load?: (path: string) => Promise<ExtractInfo>;
  taken?: (path: string) => Promise<boolean>;
  /** Already read (tests): shown at once, not read again. */
  initial?: ExtractInfo | null;
  /** Merge and replace is being confirmed already (tests). */
  confirming?: boolean;
}) {
  const [state, setState] = useState<Load>(initial ? { info: initial } : null);
  const [outName, setOutName] = useState<string | null>(null);
  const [naming, setNaming] = useState(false);
  const [nameError, setNameError] = useState<string | null>(null);
  const [confirmReplace, setConfirmReplace] = useState(confirming);
  const [deleteAfter, setDeleteAfter] = useState(false);
  const preset = initial != null;
  useEffect(() => {
    if (preset) return;
    let live = true;
    load(path).then(
      (info) => {
        if (live) setState({ info });
      },
      (e: unknown) => {
        if (!live) return;
        // A helper without the check (or a check that could not run): let the helper decide.
        const generic = e instanceof ApiError && (e.status === 0 || e.status >= 500 || e.code === "http_404");
        setState({ error: generic ? null : extractErrorText(e), generic });
      },
    );
    return () => {
      live = false;
    };
  }, [path, load, preset]);

  const info = state && "info" in state ? state.info : null;
  const generic = !!state && "generic" in state && state.generic;
  const folder = outName ?? info?.name ?? archiveFolderName(name);
  const dest = joinPath(dir, folder);
  const exists = !!info && info.exists && outName == null;
  const canStart = !busy && !naming && !exists && (info != null || generic);
  useEffect(() => {
    if (!exists) refocusIfLost(topModal(), ['[data-action="extract-go"]', "[data-nav-default]"]);
  }, [exists]);

  const keepBoth = () => {
    setNaming(true);
    setNameError(null);
    freeName(dir, folder, true, taken).then(
      (n) => {
        setNaming(false);
        setOutName(n);
      },
      (e: unknown) => {
        setNaming(false);
        setNameError(filesErrorText(e));
      },
    );
  };

  const title = t("cw_files_extract_title", { name });
  return (
    <Modal label={title} className="cw-unpack cw-extract" onBack={onCancel}>
      <h2 className="cw-sheet-title">{title}</h2>
      {state == null ? <p className="cw-dim">{t("cw_home_loading")}</p> : null}
      {state != null && (info || generic) ? (
        <>
          <p>{t("cw_files_extract_body")}</p>
          <dl className="cw-info">
            <div>
              <dt className="cw-dim">{t("cw_files_extract_into")}</dt>
              <dd className="cw-mono" data-extract-out="">
                {dest}
              </dd>
            </div>
            {info ? (
              <>
                <div>
                  <dt className="cw-dim">{t("cw_files_extract_size")}</dt>
                  <dd>
                    {info.volumes > 1
                      ? t("cw_files_extract_size_parts", { size: formatBytes(info.size), n: info.volumes })
                      : formatBytes(info.size)}
                  </dd>
                </div>
                <div>
                  <dt className="cw-dim">{t("cw_files_unpack_free")}</dt>
                  <dd>{info.free != null ? formatBytes(info.free) : "—"}</dd>
                </div>
              </>
            ) : null}
          </dl>
          <p className="cw-dim">{t("cw_files_extract_space_note")}</p>
          {exists ? (
            <p className="cw-field-error" role="alert" data-extract-blocked="exists">
              {t("cw_files_extract_exists", { name: folder })}
            </p>
          ) : null}
        </>
      ) : null}
      {state && "generic" in state && !state.generic ? (
        <p className="cw-field-error" role="alert">
          {state.error ?? t("cw_files_err_read")}
        </p>
      ) : null}
      {nameError ? (
        <p className="cw-field-error" role="alert">
          {nameError}
        </p>
      ) : null}
      {busy ? <p className="cw-dim">{t("cw_files_extract_wait_batch")}</p> : null}
      <button
        type="button"
        role="switch"
        aria-checked={deleteAfter}
        className="cw-focusable cw-button"
        data-action="extract-delete-after"
        onClick={() => setDeleteAfter((v) => !v)}
      >
        <span>{t("cw_files_extract_delete_after")}</span>
        <span className={`cw-tag${deleteAfter ? " is-on" : ""}`}>{t(deleteAfter ? "cw_settings_on" : "cw_settings_off")}</span>
      </button>
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" data-nav-default="" onClick={onCancel}>
          <span>{t("cw_dialog_cancel")}</span>
        </button>
        {exists ? (
          <>
            <button type="button" className="cw-focusable cw-button" data-action="extract-keep" aria-busy={naming || undefined} onClick={keepBoth}>
              <span>{t("cw_files_conflict_keep")}</span>
            </button>
            <button
              type="button"
              className="cw-focusable cw-button"
              data-action="extract-merge"
              disabled={busy}
              onClick={() => onStart({ dest, merge: true, replace: false, deleteAfter })}
            >
              <span>{t("cw_files_extract_merge")}</span>
            </button>
            <button
              type="button"
              className="cw-focusable cw-button is-danger"
              data-action="extract-replace"
              disabled={busy}
              onClick={() => setConfirmReplace(true)}
            >
              <span>{t("cw_files_extract_merge_replace")}</span>
            </button>
          </>
        ) : (
          <button
            type="button"
            className="cw-focusable cw-button is-primary"
            data-action="extract-go"
            disabled={!canStart}
            onClick={() => onStart({ dest, merge: false, replace: false, deleteAfter })}
          >
            <span>{t("cw_files_extract_go")}</span>
          </button>
        )}
      </div>
      {/* Over the dialog, which stays mounted: ○ returns to Merge and replace, not to a re-opened dialog. */}
      {confirmReplace ? (
        <ConfirmDialog
          title={t("cw_files_extract_replace_title", { name: folder })}
          body={t("cw_files_extract_replace_body")}
          confirmLabel={t("cw_files_extract_merge_replace")}
          danger
          onConfirm={() => onStart({ dest, merge: true, replace: true, deleteAfter })}
          onCancel={() => setConfirmReplace(false)}
        />
      ) : null}
    </Modal>
  );
}
