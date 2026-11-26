import { useEffect, useRef, useState } from "react";
import { FilePlus, FolderUp, Settings, Square, SquareCheck, X } from "lucide-react";
import { formatBytes } from "../../lib/format";
import { ApiError } from "../api";
import { t } from "../strings";
import { Modal } from "../ui/Modal";
import { PhoneHandoff } from "../ui/PhoneHandoff";
import { statOrNull, type ConflictChoice } from "./paste";
import { makeFolders, uploadFile, uploadItems, type UploadItem } from "./transfer";
import { filesErrorText } from "./Files";
import { runUploads, type UploadProgress, type UploadSummary } from "./uploadRun";

/** Can this browser pick a whole folder (webkitdirectory)? */
export function canPickFolder(doc: Document | null = typeof document !== "undefined" ? document : null): boolean {
  if (!doc) return false;
  return "webkitdirectory" in doc.createElement("input");
}

/** "Uploaded 4 · skipped 1 · 1 failed: …". */
export function uploadSummaryText(s: UploadSummary): string {
  const parts = [t("cw_files_up_done", { n: s.uploaded.length })];
  if (s.skipped.length) parts.push(t("cw_files_up_skipped", { n: s.skipped.length }));
  if (s.stopped.length) parts.push(t("cw_files_up_stopped", { n: s.stopped.length }));
  if (s.failed.length) {
    const e = s.failed[0].error;
    const why = e instanceof ApiError && e.code === "no_space" ? t("cw_files_up_no_space") : (filesErrorText(e) ?? "");
    parts.push(t("cw_files_up_failed", { n: s.failed.length, reason: why }));
  }
  return parts.join(" · ");
}

type Ask = { name: string; canReplace: boolean; left: number; answer: (d: { choice: ConflictChoice; all: boolean }) => void };

type State = { kind: "pick" } | { kind: "running"; p: UploadProgress | null } | { kind: "ended"; sum: UploadSummary };

/**
 * Upload here (a phone or PC on the LAN only): pick files, or a folder where
 * the browser can, and they are sent one at a time into the folder shown,
 * each written under a temporary name and renamed when complete (a cancel
 * leaves nothing). The current file's progress, Cancel for it, Stop for the
 * rest; a name already there asks Skip / Keep both / Replace.
 */
export function UploadDialog({
  dir,
  free,
  fsNote,
  onDone,
  onClose,
}: {
  dir: string;
  /** Free bytes on the drive, when known. */
  free: number | null;
  /** The drive keeps no permissions (exFAT/FAT): said under the buttons. */
  fsNote?: string | null;
  /** Something was uploaded (refresh the listing). */
  onDone: () => void;
  onClose: () => void;
}) {
  const [state, setState] = useState<State>({ kind: "pick" });
  const [ask, setAsk] = useState<Ask | null>(null);
  const [all, setAll] = useState(false);
  const files = useRef<HTMLInputElement>(null);
  const folder = useRef<HTMLInputElement>(null);
  const current = useRef<AbortController | null>(null);
  const stop = useRef(false);
  const live = useRef(true);
  useEffect(
    () => () => {
      live.current = false;
      stop.current = true;
      current.current?.abort();
    },
    [],
  );

  const start = (picked: UploadItem[]) => {
    if (!picked.length) return;
    stop.current = false;
    setState({ kind: "running", p: null });
    void runUploads(picked, dir, {
      mkdirs: makeFolders,
      kindAt: async (p) => {
        const s = await statOrNull(p);
        return s ? (s.kind === "dir" ? "dir" : "file") : null;
      },
      upload: (dest, file, overwrite, onProgress) => {
        const ctl = new AbortController();
        current.current = ctl;
        return uploadFile(dest, file, overwrite, (sent) => onProgress(sent), ctl.signal).finally(() => {
          if (current.current === ctl) current.current = null;
        });
      },
      decide: (name, canReplace, left) =>
        new Promise((resolve) => {
          setAll(false);
          setAsk({
            name,
            canReplace,
            left,
            answer: (d) => {
              setAsk(null);
              resolve(d);
            },
          });
        }),
      onProgress: (p) => {
        if (live.current) setState({ kind: "running", p });
      },
      stopped: () => stop.current,
    }).then(
      (sum) => {
        if (!live.current) return;
        setState({ kind: "ended", sum });
        if (sum.uploaded.length) onDone();
      },
      (e: unknown) => {
        if (!live.current) return;
        setState({ kind: "ended", sum: { uploaded: [], skipped: [], failed: [{ name: "", error: e }], stopped: [] } });
      },
    );
  };

  const onPicked = (input: HTMLInputElement | null) => {
    const list = input?.files ? uploadItems(input.files) : [];
    if (input) input.value = "";
    start(list);
  };
  const stopAll = () => {
    stop.current = true;
    current.current?.abort();
    ask?.answer({ choice: "skip", all: true });
  };
  const running = state.kind === "running";
  const back = () => (running ? stopAll() : onClose());
  const p = state.kind === "running" ? state.p : null;

  return (
    <Modal label={t("cw_files_up_title")} className="cw-upload" onBack={back}>
      <h2>{t("cw_files_up_title")}</h2>
      <p className="cw-dim cw-mono">{dir}</p>
      {state.kind === "pick" ? (
        <>
          <div className="cw-upload-pick">
            <button type="button" className="cw-focusable cw-button" data-action="up-files" data-nav-default="" onClick={() => files.current?.click()}>
              <FilePlus aria-hidden />
              <span>{t("cw_files_up_pick_files")}</span>
            </button>
            {canPickFolder() ? (
              <button type="button" className="cw-focusable cw-button" data-action="up-folder" onClick={() => folder.current?.click()}>
                <FolderUp aria-hidden />
                <span>{t("cw_files_up_pick_folder")}</span>
              </button>
            ) : null}
          </div>
          <input ref={files} type="file" multiple hidden onChange={(e) => onPicked(e.currentTarget)} />
          <input
            ref={(el) => {
              folder.current = el;
              if (el) el.setAttribute("webkitdirectory", "");
            }}
            type="file"
            hidden
            onChange={(e) => onPicked(e.currentTarget)}
          />
          <p className="cw-dim">
            {[free != null ? t("cw_files_free", { free: formatBytes(free) }) : null, t("cw_files_up_hint")].filter(Boolean).join(" · ")}
          </p>
          {fsNote ? <p className="cw-dim">{fsNote}</p> : null}
        </>
      ) : null}
      {ask ? (
        <div className="cw-msg" role="alert" data-up-conflict="">
          <p>{t("cw_files_conflict_title", { name: ask.name })}</p>
          {ask.left > 0 ? (
            <button type="button" className="cw-focusable cw-button" data-action="up-all" aria-pressed={all} onClick={() => setAll((v) => !v)}>
              {all ? <SquareCheck aria-hidden /> : <Square aria-hidden />}
              <span>{t("cw_files_conflict_all", { n: ask.left })}</span>
            </button>
          ) : null}
          <div className="cw-dialog-buttons">
            <button type="button" className="cw-focusable cw-button" data-conflict="skip" data-nav-default="" onClick={() => ask.answer({ choice: "skip", all })}>
              <span>{t("cw_files_conflict_skip")}</span>
            </button>
            <button type="button" className="cw-focusable cw-button" data-conflict="keep" onClick={() => ask.answer({ choice: "keep", all })}>
              <span>{t("cw_files_conflict_keep")}</span>
            </button>
            {ask.canReplace ? (
              <button type="button" className="cw-focusable cw-button is-danger" data-conflict="replace" onClick={() => ask.answer({ choice: "replace", all })}>
                <span>{t("cw_files_conflict_replace")}</span>
              </button>
            ) : null}
          </div>
        </div>
      ) : null}
      {running ? (
        <div className="cw-upload-run" role="status" data-up-running="">
          {p ? (
            <>
              <p>
                {t("cw_files_up_file", { at: p.at + 1, n: p.total })} <span className="cw-mono">{p.name}</span>
              </p>
              <div className="cw-bar" aria-hidden>
                <div className="cw-bar-fill" style={{ width: `${p.size ? Math.floor((p.sent / p.size) * 100) : 100}%` }} />
              </div>
              <p className="cw-dim">
                {formatBytes(p.doneBytes + p.sent)} / {formatBytes(p.totalBytes)}
              </p>
            </>
          ) : (
            <p className="cw-dim">{t("cw_home_loading")}</p>
          )}
          <div className="cw-dialog-buttons">
            <button type="button" className="cw-focusable cw-button" data-action="up-skip" disabled={!!ask} onClick={() => current.current?.abort()}>
              <X aria-hidden />
              <span>{t("cw_files_up_cancel_file")}</span>
            </button>
            <button type="button" className="cw-focusable cw-button is-danger" data-action="up-stop" onClick={stopAll}>
              <Square aria-hidden />
              <span>{t("cw_files_up_stop")}</span>
            </button>
          </div>
        </div>
      ) : null}
      {state.kind === "ended" ? (
        <p className={state.sum.failed.length ? "cw-field-error" : "cw-dim"} role="status" data-up-summary="">
          {uploadSummaryText(state.sum)}
        </p>
      ) : null}
      {!running ? (
        <div className="cw-dialog-buttons">
          {state.kind === "ended" ? (
            <button type="button" className="cw-focusable cw-button" data-action="up-more" onClick={() => setState({ kind: "pick" })}>
              <span>{t("cw_files_up_more")}</span>
            </button>
          ) : null}
          <button type="button" className="cw-focusable cw-button" data-action="up-close" data-nav-default={state.kind === "ended" ? "" : undefined} onClick={onClose}>
            <span>{t("cw_files_close")}</span>
          </button>
        </div>
      ) : null}
    </Modal>
  );
}


/**
 * On the console's own browser there is nothing to pick files from, so Files offers this instead
 * of Upload: the address of this page for a phone or PC on the same network (QR code), where
 * Files has Upload. With LAN access off, how to switch it on (and Settings, one press away).
 */
export function PhoneUploadDialog({ dir, onOpenSettings, onClose }: { dir: string; onOpenSettings?: () => void; onClose: () => void }) {
  return (
    <Modal label={t("cw_files_up_phone")} className="cw-sheet cw-phone-upload" onBack={onClose}>
      <h2 className="cw-sheet-title">{t("cw_files_up_phone")}</h2>
      <PhoneHandoff
        always
        title={t("cw_files_up_phone_title")}
        body={t("cw_files_up_phone_body", { path: dir, files: t("cw_nav_files"), upload: t("cw_files_up_title") })}
        onOff={
          <div className="cw-msg is-info" data-phone-off="">
            <p>{t("cw_files_up_phone_lan_off", { setting: t("cw_settings_lan"), settings: t("cw_nav_settings") })}</p>
            {onOpenSettings ? (
              <button type="button" className="cw-focusable cw-button is-primary" data-nav-default="" onClick={onOpenSettings}>
                <Settings aria-hidden />
                <span>{t("cw_nav_settings")}</span>
              </button>
            ) : null}
          </div>
        }
      />
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" onClick={onClose}>
          <X aria-hidden />
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
    </Modal>
  );
}
