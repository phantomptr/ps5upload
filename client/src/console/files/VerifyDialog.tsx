import { useEffect, useState } from "react";
import { formatBytes } from "../../lib/format";
import { ApiError } from "../api";
import { getLanguage, t } from "../strings";
import { Modal } from "../ui/Modal";
import { filesErrorText } from "./Files";
import { crcHex, opResult, startOp, watchOp, type OpDeps, type OpStatus } from "./opJob";

export type VerifyState =
  | { step: "hash" | "crc"; done: number; total: number; hash?: string }
  | { step: "done"; hash: string; crc: string; size: number }
  | { step: "cancelled" }
  | { step: "failed"; why: string | null };

/** Why a verify step failed, from the job's cause. */
function failText(s: OpStatus | null): string | null {
  const m = s?.message ?? "";
  if (/not_regular_file/.test(m)) return t("cw_files_verify_not_file");
  if (/bad_path/.test(m)) return t("cw_files_err_denied");
  if (/stat_failed|open_failed/.test(m)) return t("cw_files_err_missing");
  return t("cw_files_verify_failed");
}

/**
 * Run Verify for `path`: BLAKE3 (HASH) then CRC32 (CRC32), each a helper job,
 * reporting progress; `stopped()` cancels the running one. Exported for tests.
 */
export async function runVerify(
  path: string,
  onState: (s: VerifyState) => void,
  stopped: () => boolean,
  deps?: { start?: typeof startOp; op?: OpDeps },
): Promise<VerifyState> {
  const start = deps?.start ?? startOp;
  let hash = "";
  let size = 0;
  for (const step of ["hash", "crc"] as const) {
    if (stopped()) return { step: "cancelled" };
    onState(step === "hash" ? { step, done: 0, total: 0 } : { step, done: 0, total: size, hash });
    let job: string;
    try {
      job = await start(step === "hash" ? "/api/fs/hash" : "/api/fs/crc32", { path });
    } catch (e) {
      return { step: "failed", why: filesErrorText(e) };
    }
    const end = await watchOp(
      job,
      {
        stopped,
        onStatus: (s) => onState(step === "hash" ? { step, done: s.bytes_done, total: s.bytes_total } : { step, done: s.bytes_done, total: s.bytes_total, hash }),
      },
      deps?.op,
    );
    if (end.end === "cancelled") return { step: "cancelled" };
    if (end.end === "gone") return stopped() ? { step: "cancelled" } : { step: "failed", why: t("cw_files_op_gone") };
    if (end.end === "failed") return { step: "failed", why: failText(end.status) };
    if (step === "hash") {
      const r = opResult<{ hash?: unknown; size?: unknown }>(end.status);
      if (!r || typeof r.hash !== "string") return { step: "failed", why: t("cw_files_verify_failed") };
      hash = r.hash;
      size = typeof r.size === "number" ? r.size : 0;
    } else {
      const r = opResult<{ crc32?: unknown; size?: unknown }>(end.status);
      if (!r || typeof r.crc32 !== "number") return { step: "failed", why: t("cw_files_verify_failed") };
      return { step: "done", hash, crc: crcHex(r.crc32), size: typeof r.size === "number" ? r.size : size };
    }
  }
  return { step: "failed", why: null };
}

function Value({ label, value, name }: { label: string; value: string; name: string }) {
  return (
    <label className="cw-field">
      <span className="cw-dim">{label}</span>
      <input
        className="cw-focusable cw-input cw-mono"
        type="text"
        readOnly
        value={value}
        data-verify={name}
        onFocus={(e) => e.currentTarget.select()}
      />
    </label>
  );
}

/**
 * Verify a file on the console: its BLAKE3 hash and CRC32, computed by the
 * helper (no transfer). The values are in read-only text boxes (select and
 * copy them). Close (or ○) while it runs stops the running job.
 */
export function VerifyDialog({
  path,
  name,
  onClose,
  initial,
}: {
  path: string;
  name: string;
  onClose: () => void;
  /** Already ended (tests): shown as is, nothing is started. */
  initial?: VerifyState;
}) {
  const [state, setState] = useState<VerifyState>(initial ?? { step: "hash", done: 0, total: 0 });
  const preset = initial != null;
  useEffect(() => {
    if (preset) return;
    let live = true;
    // This run's own stop flag: a re-run (StrictMode, a new path) never un-stops an earlier one.
    let stopped = false;
    runVerify(
      path,
      (s) => {
        if (live) setState(s);
      },
      () => stopped,
    ).then(
      (s) => {
        if (live) setState(s);
      },
      (e: unknown) => {
        if (live) setState({ step: "failed", why: e instanceof ApiError && e.status === 401 ? null : filesErrorText(e) });
      },
    );
    return () => {
      live = false;
      // Closed while a job runs: stop it (the helper drops it within a block).
      stopped = true;
    };
  }, [path, preset]);

  const running = state.step === "hash" || state.step === "crc";
  const pct = running && state.total > 0 ? Math.min(100, Math.floor((state.done / state.total) * 100)) : null;
  return (
    <Modal label={t("cw_files_verify_title", { name })} className="cw-verify" onBack={onClose}>
      <h2 className="cw-sheet-title">{t("cw_files_verify_title", { name })}</h2>
      <p className="cw-dim cw-mono">{path}</p>
      {running ? (
        <div className="cw-msg" role="status" data-verify-step={state.step}>
          {t(state.step === "hash" ? "cw_files_verify_hashing" : "cw_files_verify_crc")}
          {pct != null ? ` · ${t("cw_home_percent", { n: pct })} · ${formatBytes(state.done)} / ${formatBytes(state.total)}` : ""}
          <div className="cw-bar" aria-hidden>
            <div className={`cw-bar-fill${pct == null ? " is-unknown" : ""}`} style={{ width: `${pct ?? 100}%` }} />
          </div>
        </div>
      ) : null}
      {state.step === "crc" && state.hash ? <Value name="blake3" label={t("cw_files_verify_blake3")} value={state.hash} /> : null}
      {state.step === "done" ? (
        <>
          <Value name="blake3" label={t("cw_files_verify_blake3")} value={state.hash} />
          <Value name="crc32" label={t("cw_files_verify_crc32")} value={state.crc} />
          <p className="cw-dim">{t("cw_files_verify_size", { size: formatBytes(state.size), bytes: state.size.toLocaleString(getLanguage()) })}</p>
        </>
      ) : null}
      {state.step === "cancelled" ? <p className="cw-dim">{t("cw_files_verify_cancelled")}</p> : null}
      {state.step === "failed" && state.why ? (
        <p className="cw-field-error" role="alert">
          {state.why}
        </p>
      ) : null}
      <div className="cw-dialog-buttons">
        <button type="button" className="cw-focusable cw-button" data-nav-default="" data-action="verify-close" onClick={onClose}>
          <span>{t(running ? "cw_dialog_cancel" : "cw_files_close")}</span>
        </button>
      </div>
    </Modal>
  );
}
