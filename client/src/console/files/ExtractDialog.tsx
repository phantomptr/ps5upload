import { useEffect, useState } from "react";
import { formatBytes } from "../../lib/format";
import { archiveFolderName, freeNote, type FreePlan } from "../../lib/archiveName";
import { ApiError } from "../api";
import type { Volume } from "../home/Home";
import { t } from "../strings";
import { topModal } from "../nav";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Modal } from "../ui/Modal";
import { refocusIfLost } from "../ui/restoreFocus";
import { filesErrorText } from "./Files";
import { joinPath, volumeOf } from "./move";
import { freeName, statOrNull } from "./paste";
import { PathPicker } from "./PathPicker";
import { checkUnavailable, extractInfo, listArchiveTop, planArchiveFree, type ExtractInfo, type TopListing } from "./opJob";

type Load = { info: ExtractInfo } | { error: string | null; generic: boolean } | null;

/** How an extract is started (the password is asked for later, only when the archive has one). */
export interface ExtractStart {
  dest: string;
  merge: boolean;
  replace: boolean;
  /** Delete the archive once every entry came out. */
  deleteAfter?: boolean;
  /** Delete the archive as it is unpacked: each part goes once everything in it is out. */
  free?: boolean;
  /** "Choose items" (#969): top-level entries of the archive left out. */
  skip?: string[];
  /** The archive's password, when "Choose items" needed it to list the archive. */
  password?: string;
}

/** "Choose items" of an archive with a password: asked for (`wrong`: the one given was refused). */
export interface ItemsPassword {
  wrong: boolean;
}

/** A listing that failed for want of the password (or a wrong one): ask for it; null otherwise. */
export function itemsPasswordAsk(e: unknown): ItemsPassword | null {
  const code = e instanceof ApiError ? e.code : "";
  if (code.startsWith("fs_extract_password_required")) return { wrong: false };
  if (code.startsWith("fs_extract_wrong_password")) return { wrong: true };
  return null;
}

/** An extract cause token (fs_extract_no_space, …) as a sentence, or null when it has none. */
export function extractCauseText(code: string): string | null {
  const c = code.replace(/^fs_extract_/, "");
  switch (c) {
    case "no_space":
      return t("cw_files_extract_err_no_space");
    case "free_refused":
      return t("cw_files_extract_err_free_refused");
    case "destination_exists":
      return t("cw_files_err_exists");
    case "busy":
      return t("cw_files_extract_err_busy");
    case "convert_busy":
      // Convert is writing a game: the drive is held by it, not by another extraction.
      return t("cw_convert_err_busy");
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

/** What happens to the archive: deleted as it unpacks (never with items left out, which would go
 *  with it unseen), else deleted once out when asked. */
export function extractAfter(freeOn: boolean, leftOut: boolean, deleteAfter: boolean): Pick<ExtractStart, "deleteAfter" | "free"> {
  return freeOn && !leftOut ? { deleteAfter: false, free: true } : { deleteAfter };
}

/** What "Delete the archive as it's unpacked" does for this archive, in plain words. */
export function freeNoteText(plan: FreePlan): string {
  const note = freeNote(plan);
  switch (note.kind) {
    case "parts":
      return t("cw_files_extract_free_parts", { n: note.parts, size: formatBytes(note.saves) });
    case "parts_plain":
      return t("cw_files_extract_free_parts_plain", { n: note.parts });
    case "other_drive":
      return t("cw_files_extract_free_other_drive");
    case "end":
      return t("cw_files_extract_free_end");
    case "refused":
      return t("cw_files_extract_err_free_refused");
  }
}

/** Why /api/fs/extract/info or /api/fs/extract refused, as a sentence (null: the reload banner says it). */
export function extractErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    const why = extractCauseText(err.code.replace(/[:\s].*$/, ""));
    if (why) return why;
  }
  return filesErrorText(err);
}

/** Whether something is at `p` already. */
async function isTaken(p: string): Promise<boolean> {
  return (await statOrNull(p)) != null;
}

/**
 * Extract an archive on the console into a new folder (named after it; the
 * archive stays): next to it, or in a folder picked with Change… on any
 * drive. Before it starts: where it writes, the archive's size and parts,
 * the free space there. A folder already there offers Keep
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
  taken = isTaken,
  initial = null,
  confirming = false,
  volumes = [],
  startParent = null,
  listTop = listArchiveTop,
  startItems = null,
  startItemsPw = null,
  planFree = planArchiveFree,
  startFree = false,
  startFreePlan,
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
  /** The console's drives: where Change… starts, and the free space of another drive. */
  volumes?: Volume[];
  /** Another folder to extract into, already picked (tests). */
  startParent?: string | null;
  /** Reads the archive's top-level names for "Choose items" (tests pass their own). */
  listTop?: (path: string, dir: string, password?: string) => Promise<TopListing>;
  /** Items already listed (tests). */
  startItems?: TopListing | null;
  /** The archive asked for a password to list it (tests). */
  startItemsPw?: ItemsPassword | null;
  /** How the archive would be deleted as it unpacks into a folder (tests pass their own). */
  planFree?: (path: string, dir: string, password?: string) => Promise<FreePlan | null>;
  /** "Delete the archive as it's unpacked" already on, and what the helper said (tests). */
  startFree?: boolean;
  startFreePlan?: FreePlan | null;
}) {
  // "Choose items" (#969): the archive's top-level entries, and those left unticked.
  const [items, setItems] = useState<TopListing | null>(startItems);
  const [itemsLoading, setItemsLoading] = useState(false);
  const [itemsError, setItemsError] = useState<string | null>(null);
  const [skipped, setSkipped] = useState<Set<string>>(new Set());
  // A password-protected archive lists only with its password: asked here, and the extract then
  // uses the same one (no second question).
  const [itemsPw, setItemsPw] = useState<ItemsPassword | null>(startItemsPw);
  const [pwText, setPwText] = useState("");
  const [listedPw, setListedPw] = useState<string | null>(null);
  const skip = {
    ...(items && skipped.size > 0 ? { skip: items.top.filter((n) => skipped.has(n)) } : {}),
    ...(listedPw ? { password: listedPw } : {}),
  };
  const noneChosen = !!items && !items.more && items.top.length > 0 && items.top.every((n) => skipped.has(n));
  const loadItems = () => {
    const pw = itemsPw ? pwText : "";
    if (itemsPw && !pw) return;
    setItemsLoading(true);
    setItemsError(null);
    listTop(path, dir, pw || undefined).then(
      (l) => {
        setItemsLoading(false);
        setItems(l);
        setSkipped(new Set());
        setItemsPw(null);
        setListedPw(pw || null);
      },
      (e: unknown) => {
        setItemsLoading(false);
        const ask = itemsPasswordAsk(e);
        if (ask) {
          setItemsPw(ask);
          return;
        }
        // A helper too old to list says so as a refused folder: the plain sentence fits better.
        const code = e instanceof ApiError ? e.code : "";
        setItemsError(
          code.startsWith("fs_extract_list") || code.startsWith("fs_extract_destination_exists")
            ? t("cw_files_err_read")
            : (extractErrorText(e) ?? t("cw_files_err_read")),
        );
      },
    );
  };
  const toggleItem = (n: string) =>
    setSkipped((cur) => {
      const next = new Set(cur);
      if (next.has(n)) next.delete(n);
      else next.add(n);
      return next;
    });
  const [state, setState] = useState<Load>(initial ? { info: initial } : null);
  const [outName, setOutName] = useState<string | null>(null);
  const [naming, setNaming] = useState(false);
  const [nameError, setNameError] = useState<string | null>(null);
  const [confirmReplace, setConfirmReplace] = useState(confirming);
  const [deleteAfter, setDeleteAfter] = useState(false);
  // "Delete the archive as it's unpacked": off for every archive, and what the helper said of it.
  const [freeOn, setFreeOn] = useState(startFree);
  const [freePlan, setFreePlan] = useState<{ plan: FreePlan | null } | { error: string } | null>(
    startFreePlan !== undefined ? { plan: startFreePlan } : null,
  );
  // Where the new folder goes: the archive's folder unless Change… picked another.
  const [parent, setParent] = useState(startParent ?? dir);
  const [picking, setPicking] = useState(false);
  // Another folder: whether the new folder's name is taken there (null while checking).
  const [otherTaken, setOtherTaken] = useState<boolean | null>(null);
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
        const generic = checkUnavailable(e);
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
  const elsewhere = parent !== dir;
  const dest = joinPath(parent, folder);
  useEffect(() => {
    if (!elsewhere) return;
    let live = true;
    setOtherTaken(null);
    taken(dest).then(
      (yes) => {
        if (live) setOtherTaken(yes);
      },
      () => {
        // Not readable: let the helper decide (it refuses a folder already there).
        if (live) setOtherTaken(false);
      },
    );
    return () => {
      live = false;
    };
  }, [elsewhere, dest, taken]);
  const exists = outName == null && (elsewhere ? otherTaken === true : !!info && info.exists);
  const checking = elsewhere && otherTaken == null;
  const canStart = !busy && !naming && !exists && !checking && !noneChosen && (info != null || generic);
  // Items left out would go with the archive, unseen: freeing then is not offered.
  const leftOut = skipped.size > 0;
  const freeing = freeOn && !leftOut;
  useEffect(() => {
    if (!freeing) return;
    let live = true;
    setFreePlan(null);
    planFree(path, parent, listedPw ?? undefined).then(
      (plan) => {
        if (live) setFreePlan({ plan });
      },
      (e: unknown) => {
        if (!live) return;
        const ask = itemsPasswordAsk(e);
        setFreePlan({ error: ask ? t("cw_files_extract_free_password") : (extractErrorText(e) ?? t("cw_files_err_read")) });
      },
    );
    return () => {
      live = false;
    };
  }, [freeing, path, parent, listedPw, planFree]);
  const after = extractAfter(freeOn, leftOut, deleteAfter);
  const otherVolume = elsewhere ? volumeOf(parent, volumes) : null;
  const free = elsewhere ? (otherVolume ? otherVolume.free_bytes : null) : info ? info.free : null;
  useEffect(() => {
    if (!exists && !picking) refocusIfLost(topModal(), ['[data-action="extract-go"]', '[data-action="extract-to"]', "[data-nav-default]"]);
  }, [exists, picking]);

  const pickParent = (d: string) => {
    setPicking(false);
    setOutName(null);
    setNameError(null);
    setParent(d);
  };

  const keepBoth = () => {
    setNaming(true);
    setNameError(null);
    freeName(parent, folder, true, taken).then(
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
          <p>{t("cw_files_extract_body_v2")}</p>
          <dl className="cw-info">
            <div>
              <dt className="cw-dim">{t("cw_files_extract_into")}</dt>
              <dd className="cw-mono" data-extract-out="">
                {dest}
              </dd>
            </div>
            {elsewhere || (info && free != null) ? (
              <div>
                <dt className="cw-dim">{t("cw_files_unpack_free")}</dt>
                <dd data-extract-free="">{free != null ? formatBytes(free) : "—"}</dd>
              </div>
            ) : null}
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
              </>
            ) : null}
          </dl>
          <button type="button" className="cw-focusable cw-button" data-action="extract-to" onClick={() => setPicking(true)}>
            <span>{t("cw_files_extract_change")}</span>
          </button>
          {elsewhere ? (
            <button type="button" className="cw-focusable cw-button" data-action="extract-beside" onClick={() => pickParent(dir)}>
              <span>{t("cw_files_extract_beside")}</span>
            </button>
          ) : null}
          {items == null ? (
            <>
              {itemsPw ? (
                <div data-extract-items-pw="">
                  <p className={itemsPw.wrong ? "cw-field-error" : "cw-dim"} role={itemsPw.wrong ? "alert" : undefined}>
                    {t(itemsPw.wrong ? "cw_files_extract_pw_wrong" : "cw_files_extract_pw_body")}
                  </p>
                  <label className="cw-field">
                    <span className="cw-dim">{t("cw_files_extract_pw_label")}</span>
                    <input
                      className="cw-focusable cw-input"
                      type="password"
                      autoComplete="off"
                      value={pwText}
                      onChange={(e) => setPwText(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          loadItems();
                        }
                      }}
                      data-extract-items-password=""
                    />
                  </label>
                </div>
              ) : null}
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="extract-choose"
                aria-busy={itemsLoading || undefined}
                disabled={itemsLoading || (!!itemsPw && !pwText)}
                onClick={loadItems}
              >
                <span>{itemsLoading ? t("cw_home_loading") : t("cw_files_extract_choose")}</span>
              </button>
            </>
          ) : (
            <div className="cw-extract-items" data-extract-items="">
              <p className="cw-dim">
                {t("cw_files_status_filtered", {
                  n: items.top.filter((n) => !skipped.has(n)).length,
                  total: items.top.length,
                })}
              </p>
              <button type="button" className="cw-focusable cw-button" data-action="extract-items-all" onClick={() => setSkipped(new Set())}>
                <span>{t("cw_files_select_all")}</span>
              </button>
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="extract-items-none"
                onClick={() => setSkipped(new Set(items.top))}
              >
                <span>{t("cw_files_select_none")}</span>
              </button>
              {items.top.map((n) => {
                const on = !skipped.has(n);
                return (
                  <button
                    key={n}
                    type="button"
                    role="checkbox"
                    aria-checked={on}
                    className="cw-focusable cw-button"
                    data-extract-item={n}
                    onClick={() => toggleItem(n)}
                  >
                    <span className="cw-mono">{items.dirs.includes(n) ? `${n}/` : n}</span>
                    <span className={`cw-tag${on ? " is-on" : ""}`}>{t(on ? "cw_settings_on" : "cw_settings_off")}</span>
                  </button>
                );
              })}
            </div>
          )}
          {itemsError ? (
            <p className="cw-field-error" role="alert" data-extract-items-error="">
              {itemsError}
            </p>
          ) : null}
          {checking ? <p className="cw-dim">{t("cw_home_loading")}</p> : null}
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
        aria-checked={freeing}
        className="cw-focusable cw-button"
        data-action="extract-free"
        disabled={leftOut}
        onClick={() => setFreeOn((v) => !v)}
      >
        <span>{t("cw_files_extract_free")}</span>
        <span className={`cw-tag${freeing ? " is-on" : ""}`}>{t(freeing ? "cw_settings_on" : "cw_settings_off")}</span>
      </button>
      <p className="cw-dim" data-extract-free-hint="">
        {leftOut ? t("cw_files_extract_free_items") : t("cw_files_extract_free_hint")}
      </p>
      {freeing ? (
        freePlan == null ? (
          <p className="cw-dim" data-extract-free-note="checking">
            {t("cw_files_extract_free_checking")}
          </p>
        ) : "error" in freePlan ? (
          <p className="cw-field-error" role="alert" data-extract-free-note="error">
            {freePlan.error}
          </p>
        ) : freePlan.plan == null ? (
          <p className="cw-field-error" role="alert" data-extract-free-note="old">
            {t("cw_files_extract_free_old")}
          </p>
        ) : (
          <p
            className={freeNote(freePlan.plan).kind.startsWith("parts") ? "cw-dim" : "cw-field-error"}
            data-extract-free-note={freeNote(freePlan.plan).kind}
          >
            {freeNoteText(freePlan.plan)}
          </p>
        )
      ) : (
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
      )}
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
              onClick={() => onStart({ dest, merge: true, replace: false, ...after, ...skip })}
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
            onClick={() => onStart({ dest, merge: false, replace: false, ...after, ...skip })}
          >
            <span>{t("cw_files_extract_go")}</span>
          </button>
        )}
      </div>
      {/* Over the dialog, which stays mounted: ○ returns to it with what was chosen. */}
      {picking ? (
        <PathPicker
          title={t("cw_files_extract_to")}
          subtitle={name}
          volumes={volumes}
          start={parent}
          onPick={pickParent}
          onCancel={() => setPicking(false)}
        />
      ) : null}
      {/* Over the dialog, which stays mounted: ○ returns to Merge and replace, not to a re-opened dialog. */}
      {confirmReplace ? (
        <ConfirmDialog
          title={t("cw_files_extract_replace_title", { name: folder })}
          body={t("cw_files_extract_replace_body")}
          confirmLabel={t("cw_files_extract_merge_replace")}
          danger
          onConfirm={() => onStart({ dest, merge: true, replace: true, ...after, ...skip })}
          onCancel={() => setConfirmReplace(false)}
        />
      ) : null}
    </Modal>
  );
}
