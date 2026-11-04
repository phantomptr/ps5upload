import { useEffect, useMemo, useRef, useState } from "react";
import { File, RotateCw, Search, Square } from "lucide-react";
import { formatBytes } from "../../../lib/format";
import { errorText, type Volume } from "../../home/Home";
import { getLanguage, t } from "../../strings";
import { Modal } from "../../ui/Modal";
import {
  createSearchFlow,
  driveRoots,
  INITIAL_SEARCH,
  namePattern,
  searchApi,
  type Hit,
  type Scope,
  type SearchFlow,
  type SearchState,
} from "./search";
import "./search.css";

/** The index line: none yet, building (with its count), or ready (and how complete it is). */
export function indexText(s: SearchState): string | null {
  if (s.unsupported) return t("cw_files_search_unsupported");
  const i = s.index;
  if (!i) return t("cw_home_loading");
  const n = i.files.toLocaleString(getLanguage());
  if (i.phase === "building") return s.stopping ? t("cw_files_search_stopping") : t("cw_files_search_building", { n });
  if (i.phase === "idle" || (i.phase === "ready" && !i.roots)) return t("cw_files_search_no_index");
  const notes = [t("cw_files_search_ready", { n })];
  if (i.cancelled) notes.push(t("cw_files_search_partial"));
  else if (i.truncated) notes.push(t("cw_files_search_capped"));
  return notes.join(" ");
}

/** The results line: how many, whether there are more, or nothing found. */
export function resultsText(s: SearchState): string | null {
  if (s.searching) return t("cw_files_search_searching");
  if (!s.done) return null;
  const n = s.done.results.hits.length;
  // Nothing here, but the helper stopped at its limit: matches elsewhere filled it (an older helper
  // filters "This folder" after its limit). Not "nothing matches".
  if (!n && s.done.results.truncated) return t("cw_files_search_none_capped");
  if (!n) return t("cw_files_search_none", { q: s.done.pattern });
  return s.done.results.truncated ? t("cw_files_search_more", { n }) : t("cw_files_search_found", { n });
}

/** The props-only view (tests render it). */
export function SearchView({
  state,
  ready = true,
  field,
  scope,
  folder,
  onField,
  onScope,
  onSubmit,
  onCancel,
  onRebuild,
  onReveal,
  onClose,
}: {
  state: SearchState;
  /** The scope's roots are known (the drives have loaded): Search can run. */
  ready?: boolean;
  field: string;
  scope: Scope;
  folder: string;
  onField: (v: string) => void;
  onScope: (s: Scope) => void;
  onSubmit: () => void;
  onCancel: () => void;
  onRebuild: () => void;
  onReveal: (hit: Hit) => void;
  onClose: () => void;
}) {
  const building = state.index?.phase === "building";
  const bad = field.trim() !== "" && namePattern(field) == null;
  const idx = indexText(state);
  const res = resultsText(state);
  const err = state.error ? errorText(state.error) : null;
  return (
    <>
      <div className="cw-viewer-head">
        <h2 className="cw-file-name">{t("cw_files_search")}</h2>
        <button type="button" className="cw-focusable cw-button" data-action="search-close" onClick={onClose}>
          <span>{t("cw_files_close")}</span>
        </button>
      </div>
      <form
        className="cw-tv-bar cw-search-bar"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          if (!bad && ready) onSubmit();
        }}
      >
        <label className="cw-tv-find">
          <Search aria-hidden />
          <input
            className="cw-focusable cw-input cw-search-field"
            type="text"
            value={field}
            placeholder={t("cw_files_search_field")}
            aria-label={t("cw_files_search_field")}
            data-nav-default=""
            data-action="search-field"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            disabled={state.unsupported}
            onChange={(e) => onField(e.target.value)}
          />
        </label>
        <button
          type="submit"
          className="cw-focusable cw-button is-primary"
          data-action="search-go"
          disabled={!ready || state.unsupported || !field.trim() || bad || state.searching}
        >
          <span>{t("cw_files_search_go")}</span>
        </button>
        <span className="cw-search-scope" role="group" aria-label={t("cw_files_search_scope")}>
          <button
            type="button"
            className="cw-focusable cw-button"
            data-scope="all"
            aria-pressed={scope === "all"}
            onClick={() => onScope("all")}
          >
            <span>{t("cw_files_search_all")}</span>
          </button>
          <button
            type="button"
            className="cw-focusable cw-button"
            data-scope="folder"
            aria-pressed={scope === "folder"}
            onClick={() => onScope("folder")}
          >
            <span>{t("cw_files_search_here")}</span>
          </button>
        </span>
      </form>
      <p className="cw-dim cw-search-hint">
        {scope === "folder" ? <span className="cw-mono">{folder} · </span> : null}
        {t("cw_files_search_hint")}
      </p>
      {bad ? (
        <p className="cw-field-error" role="alert">
          {t("cw_files_search_bad")}
        </p>
      ) : null}
      <div className="cw-msg cw-search-index" role="status" data-index-phase={state.index?.phase ?? ""}>
        <span>{idx}</span>
        {building && !state.stopping ? (
          <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="index-cancel" onClick={onCancel}>
            <Square aria-hidden />
            <span>{t("cw_files_search_cancel")}</span>
          </button>
        ) : null}
        {!building && state.index?.phase === "ready" && state.index.roots ? (
          <button type="button" className="cw-focusable cw-button cw-msg-action" data-action="index-rebuild" onClick={onRebuild}>
            <RotateCw aria-hidden />
            <span>{t("cw_files_search_rebuild")}</span>
          </button>
        ) : null}
        {building ? (
          <div className="cw-bar" aria-hidden>
            <div className="cw-bar-fill is-unknown" style={{ width: "100%" }} />
          </div>
        ) : null}
        {state.pending && building ? <span className="cw-dim cw-search-pending">{t("cw_files_search_waiting", { q: state.pending })}</span> : null}
      </div>
      {err ? (
        <p className="cw-field-error" role="alert">
          {err}
        </p>
      ) : null}
      <div className="cw-search-body">
        {res ? (
          <p className="cw-dim" role="status" data-results-line="">
            {res}
          </p>
        ) : null}
        {state.done && state.done.results.hits.length ? (
          <ul className="cw-files" data-nav-list aria-label={t("cw_files_search_results")}>
            {state.done.results.hits.map((h) => (
              <li key={h.path} className="cw-file is-plain" data-nav-item>
                <button type="button" className="cw-focusable cw-file-main" data-hit={h.path} onClick={() => onReveal(h)}>
                  <File className="cw-file-icon" aria-hidden />
                  <span className="cw-file-text">
                    <span className="cw-file-name">{h.name}</span>
                    <span className="cw-dim cw-mono cw-file-sub">{h.folder}</span>
                  </span>
                  <span className="cw-file-meta cw-dim">{formatBytes(h.size)}</span>
                </button>
              </li>
            ))}
          </ul>
        ) : null}
      </div>
    </>
  );
}

/**
 * Search (a Files tool): a name or pattern, all drives or this folder. The first
 * search builds the helper's index for that scope (progress, Cancel); results
 * open in Files at the item (✕). Closing leaves a running build to finish in the
 * helper (the next search finds it ready).
 */
export function SearchDialog({
  folder,
  volumes,
  onReveal,
  onClose,
  flowFactory,
}: {
  folder: string;
  volumes: Volume[];
  onReveal: (dir: string, name: string) => void;
  onClose: () => void;
  /** Tests: a flow over fake calls. */
  flowFactory?: (onChange: (s: SearchState) => void) => SearchFlow;
}) {
  const [state, setState] = useState<SearchState>(INITIAL_SEARCH);
  const [field, setField] = useState("");
  const [scope, setScope] = useState<Scope>("all");
  const flow = useRef<SearchFlow | null>(null);
  const roots = useMemo(() => (scope === "all" ? driveRoots(volumes) : [folder]), [scope, volumes, folder]);
  useEffect(() => {
    const f = flowFactory
      ? flowFactory(setState)
      : createSearchFlow(
          { api: searchApi, setTimer: (fn, ms) => setTimeout(fn, ms), clearTimer: (x) => clearTimeout(x as ReturnType<typeof setTimeout>) },
          setState,
        );
    flow.current = f;
    void f.open();
    return () => f.close();
  }, [flowFactory]);
  return (
    <Modal label={t("cw_files_search")} className="cw-viewer cw-search" onBack={onClose}>
      <SearchView
        state={state}
        ready={roots.length > 0}
        field={field}
        scope={scope}
        folder={folder}
        onField={setField}
        onScope={setScope}
        onSubmit={() => void flow.current?.submit(field, scope, roots, scope === "folder" ? folder : null)}
        onCancel={() => void flow.current?.cancel()}
        onRebuild={() => {
          if (roots.length) void flow.current?.rebuild(roots);
        }}
        onReveal={(h) => onReveal(h.folder, h.name)}
        onClose={onClose}
      />
    </Modal>
  );
}
