import { useEffect, useState } from "react";
import { formatBytes } from "../../lib/format";
import { ApiError } from "../api";
import { t } from "../strings";
import { topModal } from "../nav";
import { Modal } from "../ui/Modal";
import { refocusIfLost } from "../ui/restoreFocus";
import { filesErrorText } from "./Files";
import { freeName, statOrNull } from "./paste";
import { checkUnavailable, unpackBlocker, unpackInfo, type UnpackInfo } from "./opJob";

type Load = { info: UnpackInfo } | { error: string | null; generic: boolean } | null;

/** A decompress cause token (the unpack's own: fs_unpack_no_space, …) as a sentence, or null when it has none. */
export function unpackCauseText(code: string): string | null {
  if (/not_pfs|not_pfsc|bad_container|bad_directory|bad_inode|bad_name|truncated/.test(code)) return t("cw_files_unpack_not_container");
  if (/no_space/.test(code)) return t("cw_files_unpack_no_space_short");
  if (/destination_exists/.test(code)) return t("cw_files_err_exists");
  if (/unpack_busy/.test(code)) return t("cw_files_unpack_busy");
  if (/not_exfat/.test(code)) return t("cw_files_unpack_not_image");
  if (/corrupt_block|inner_missing|size_mismatch/.test(code)) return t("cw_files_unpack_damaged");
  if (/path_not_allowed/.test(code)) return t("cw_files_err_denied");
  return null;
}

/** Why /api/fs/unpack/info or /api/fs/unpack refused the file, as a sentence (null: the reload banner says it). */
export function unpackErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    const why = unpackCauseText(err.code);
    if (why) return why;
  }
  return filesErrorText(err);
}

/**
 * Decompress a .ffpfsc: the image it holds is written next to it (the .ffpfsc
 * stays). Before it starts: what it writes and how big, the free space there
 * (refused when short: the image plus 1 GiB), and a name already taken
 * (refused, with Keep both: "name (2).exfat"). Start hands it to Files, which
 * runs it as the page's one batch (progress, Stop).
 */
export function UnpackDialog({
  dir,
  name,
  path,
  busy,
  onStart,
  onCancel,
  load = unpackInfo,
  taken = async (p: string) => (await statOrNull(p)) != null,
  initial = null,
  initialName = null,
}: {
  dir: string;
  name: string;
  path: string;
  /** Another batch runs: Start waits. */
  busy: boolean;
  /** Start with the image's own name (null) or the Keep both name. */
  onStart: (outName: string | null, info: UnpackInfo | null) => void;
  onCancel: () => void;
  load?: (path: string) => Promise<UnpackInfo>;
  taken?: (path: string) => Promise<boolean>;
  /** Already read (tests): shown at once, not read again. */
  initial?: UnpackInfo | null;
  initialName?: string | null;
}) {
  const [state, setState] = useState<Load>(initial ? { info: initial } : null);
  const [outName, setOutName] = useState<string | null>(initialName);
  const [naming, setNaming] = useState(false);
  const [nameError, setNameError] = useState<string | null>(null);
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
        // A helper without the check (or a check that could not run): warn and let the helper decide.
        const generic = checkUnavailable(e);
        setState({ error: generic ? null : unpackErrorText(e), generic });
      },
    );
    return () => {
      live = false;
    };
  }, [path, load, preset]);

  const info = state && "info" in state ? state.info : null;
  const blocker = info ? unpackBlocker(info, outName) : null;
  const canStart = !busy && !naming && (info ? blocker == null : !!state && "generic" in state && state.generic);
  // Keep both removes itself once a free name is found: focus moves on to Decompress (else Cancel).
  const keepShown = blocker === "exists";
  useEffect(() => {
    if (!keepShown) refocusIfLost(topModal(), ['[data-action="unpack-go"]', "[data-nav-default]"]);
  }, [keepShown]);
  const keepBoth = () => {
    if (!info) return;
    setNaming(true);
    setNameError(null);
    freeName(dir, info.name, false, taken).then(
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

  const title = t("cw_files_unpack_title", { name });
  return (
    <Modal label={title} className="cw-unpack" onBack={onCancel}>
      <h2 className="cw-sheet-title">{title}</h2>
      {state == null ? <p className="cw-dim">{t("cw_home_loading")}</p> : null}
      {info ? (
        <>
          <p>{t("cw_files_unpack_body", { name: outName ?? info.name, size: formatBytes(info.size) })}</p>
          <dl className="cw-info">
            <div>
              <dt className="cw-dim">{t("cw_files_unpack_writes")}</dt>
              <dd className="cw-mono" data-unpack-out="">
                {`${dir.replace(/\/+$/, "")}/${outName ?? info.name}`}
              </dd>
            </div>
            <div>
              <dt className="cw-dim">{t("cw_files_unpack_needs")}</dt>
              <dd>{formatBytes(info.need)}</dd>
            </div>
            <div>
              <dt className="cw-dim">{t("cw_files_unpack_free")}</dt>
              <dd>{info.free != null ? formatBytes(info.free) : "—"}</dd>
            </div>
          </dl>
          {info.free == null ? <p className="cw-dim">{t("cw_files_unpack_free_unknown")}</p> : null}
          {blocker === "space" ? (
            <p className="cw-field-error" role="alert" data-unpack-blocked="space">
              {t("cw_files_unpack_no_space", { need: formatBytes(info.need), free: formatBytes(info.free ?? 0) })}
            </p>
          ) : null}
          {blocker === "exists" ? (
            <p className="cw-field-error" role="alert" data-unpack-blocked="exists">
              {t("cw_files_unpack_exists", { name: info.name })}
            </p>
          ) : null}
        </>
      ) : null}
      {state && "generic" in state ? (
        state.generic ? (
          <p className="cw-dim" data-unpack-generic="">
            {t("cw_files_unpack_generic")}
          </p>
        ) : (
          <p className="cw-field-error" role="alert">
            {state.error ?? t("cw_files_err_read")}
          </p>
        )
      ) : null}
      {nameError ? (
        <p className="cw-field-error" role="alert">
          {nameError}
        </p>
      ) : null}
      {busy ? <p className="cw-dim">{t("cw_files_unpack_wait_batch")}</p> : null}
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" data-nav-default="" onClick={onCancel}>
          <span>{t("cw_dialog_cancel")}</span>
        </button>
        {blocker === "exists" ? (
          <button type="button" className="cw-focusable cw-button" data-action="unpack-keep" aria-busy={naming || undefined} onClick={keepBoth}>
            <span>{t("cw_files_conflict_keep")}</span>
          </button>
        ) : null}
        <button
          type="button"
          className="cw-focusable cw-button is-primary"
          data-action="unpack-go"
          disabled={!canStart}
          onClick={() => onStart(outName, info)}
        >
          <span>{t("cw_files_unpack_go")}</span>
        </button>
      </div>
    </Modal>
  );
}
