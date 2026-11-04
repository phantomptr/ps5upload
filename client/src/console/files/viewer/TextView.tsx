import { memo, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronUp, Search, WrapText } from "lucide-react";
import { getLanguage, t } from "../../strings";
import {
  BLOCK_ROWS,
  findMatches,
  firstMatchFrom,
  highlight,
  ROW_WINDOW,
  stepMatch,
  windowAround,
  type Match,
  type Row,
  type RowWindow,
} from "./text";
import "./viewer.css";

/** "3 of 17", "No matches", "10,000+ matches" (null before the first find). */
export function findCountText(query: string, count: number, current: number, capped: boolean): string | null {
  if (!query) return null;
  if (!count) return t("cw_files_vw_find_none");
  if (capped && current < 0) return t("cw_files_vw_find_many", { n: count.toLocaleString(getLanguage()) });
  const at = t("cw_files_vw_find_at", { at: current + 1, n: count.toLocaleString(getLanguage()) });
  return capped ? `${at} · ${t("cw_files_vw_find_capped")}` : at;
}

type Mark = { start: number; end: number; index: number };

interface Found {
  query: string;
  matches: Match[];
  capped: boolean;
}

const NONE: Found = { query: "", matches: [], capped: false };

/** A pause in typing this long finds what the field holds (the count shows without pressing Next). */
export const FIND_TYPED_MS = 350;

export interface FindState {
  found: Found;
  current: number;
  win: RowWindow;
}

/**
 * Find `field` as typed: a new query is scanned and its first match from the
 * window's top becomes current; an empty field clears the find; the query
 * already found is left as it is (Next / Previous step through it).
 */
export function findTyped(s: FindState, field: string, rows: Row[]): FindState {
  if (field === s.found.query) return s;
  if (!field) return { ...s, found: NONE, current: -1 };
  const found = { query: field, ...findMatches(rows, field) };
  const current = firstMatchFrom(found.matches, s.win.start);
  const win = current >= 0 ? windowAround(found.matches[current].row, s.win, rows.length) : s.win;
  return { found, current, win };
}

/** One focusable block of rows. Memoized: Next / Previous re-render only the blocks whose current mark changes. */
const Block = memo(function Block({
  rows,
  first,
  marks,
  current,
}: {
  rows: Row[];
  /** Index of its first row. */
  first: number;
  /** Its rows' matches, by row index (stable while the query is). */
  marks: Map<number, Mark[]> | undefined;
  /** The current match's index when it is in this block, else -1. */
  current: number;
}) {
  return (
    <div className="cw-focusable cw-tv-block" tabIndex={0}>
      {rows.map((r, ri) => {
        const m = marks?.get(first + ri);
        return (
          <div key={ri} className="cw-tv-row">
            <span className="cw-tv-n" aria-hidden>
              {r.cont ? "" : r.line}
            </span>
            <span className="cw-tv-t">
              {m
                ? highlight(r.text, m).map(([s, mi], k) =>
                    mi < 0 ? (
                      s
                    ) : (
                      <mark key={k} data-match={mi} className={mi === current ? "is-current" : undefined}>
                        {s}
                      </mark>
                    ),
                  )
                : r.text}
            </span>
          </div>
        );
      })}
    </div>
  );
});

/**
 * Numbered rows of text with find (the field, then Previous / Next: on-screen
 * buttons, ✕ on each) and a wrap toggle. A window of rows is drawn (ROW_WINDOW
 * at first; "Show more" / "Show earlier" extend it; a find jump outside it
 * draws a fresh window around the match), in memoized focusable blocks the
 * D-pad steps through. Text is rendered as text nodes, never HTML.
 */
export function TextView({ rows, initialQuery }: { rows: Row[]; /** Tests: start with this found. */ initialQuery?: string }) {
  const [state, setState] = useState<FindState>(() => {
    const found = initialQuery ? { query: initialQuery, ...findMatches(rows, initialQuery) } : NONE;
    const current = found.matches.length ? 0 : -1;
    const win0: RowWindow = { start: 0, end: Math.min(rows.length, ROW_WINDOW) };
    const win = current >= 0 ? windowAround(found.matches[0].row, win0, rows.length) : win0;
    return { found, current, win };
  });
  const [field, setField] = useState(initialQuery ?? "");
  const [wrap, setWrap] = useState(true);
  const body = useRef<HTMLDivElement>(null);
  const { found, current, win } = state;
  const { matches, capped } = found;

  /** Every block of the file (slices made once per file). */
  const blocks = useMemo(() => {
    const out: Row[][] = [];
    for (let i = 0; i < rows.length; i += BLOCK_ROWS) out.push(rows.slice(i, i + BLOCK_ROWS));
    return out;
  }, [rows]);

  /** Matches by block, then by row (once per query). */
  const byBlock = useMemo(() => {
    const m = new Map<number, Map<number, Mark[]>>();
    matches.forEach((x, index) => {
      const b = Math.floor(x.row / BLOCK_ROWS);
      let rowsOf = m.get(b);
      if (!rowsOf) m.set(b, (rowsOf = new Map()));
      const list = rowsOf.get(x.row);
      const mark = { start: x.start, end: x.end, index };
      if (list) list.push(mark);
      else rowsOf.set(x.row, [mark]);
    });
    return m;
  }, [matches]);

  // The current match into view (focus stays on Next / Previous).
  useEffect(() => {
    if (current < 0) return;
    const el = body.current?.querySelector<HTMLElement>(`[data-match="${current}"]`);
    if (el && typeof el.scrollIntoView === "function") el.scrollIntoView({ block: "center", inline: "nearest" });
  }, [current, win]);

  // Typing pauses: find it, so the count shows at once (the PS5 keyboard's Done may not submit).
  useEffect(() => {
    if (field === found.query) return;
    const id = setTimeout(() => setState((s) => findTyped(s, field, rows)), FIND_TYPED_MS);
    return () => clearTimeout(id);
  }, [field, found.query, rows]);

  /** Find what the field holds: a new query (scanned once) starts at its first match; the same one steps on. */
  const go = (dir: 1 | -1) => {
    setState((s) => {
      let f = s.found;
      let cur: number;
      if (field !== f.query) {
        f = { query: field, ...findMatches(rows, field) };
        cur = dir === 1 ? firstMatchFrom(f.matches, s.win.start) : f.matches.length - 1;
      } else {
        cur = stepMatch(s.current, f.matches.length, dir);
      }
      const w = cur >= 0 ? windowAround(f.matches[cur].row, s.win, rows.length) : s.win;
      return { found: f, current: cur, win: w };
    });
  };
  const setWin = (w: RowWindow) => setState((s) => ({ ...s, win: w }));

  const firstBlock = Math.floor(win.start / BLOCK_ROWS);
  const lastBlock = Math.ceil(win.end / BLOCK_ROWS);
  const curBlock = current >= 0 ? Math.floor(matches[current].row / BLOCK_ROWS) : -1;
  const gutter = String(rows.length ? rows[rows.length - 1].line : 1).length;
  const count = findCountText(found.query, matches.length, current, capped);

  return (
    <div className="cw-tv">
      <form
        className="cw-tv-bar"
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          go(1);
        }}
      >
        <label className="cw-tv-find">
          <Search aria-hidden />
          <input
            className="cw-focusable cw-input"
            type="text"
            value={field}
            placeholder={t("cw_files_vw_find")}
            aria-label={t("cw_files_vw_find")}
            data-action="find-field"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            onChange={(e) => setField(e.target.value)}
          />
        </label>
        <button type="button" className="cw-focusable cw-button" data-action="find-prev" disabled={!field} onClick={() => go(-1)}>
          <ChevronUp aria-hidden />
          <span>{t("cw_files_vw_prev")}</span>
        </button>
        <button type="submit" className="cw-focusable cw-button" data-action="find-next" disabled={!field}>
          <ChevronDown aria-hidden />
          <span>{t("cw_files_vw_next")}</span>
        </button>
        <span className="cw-dim cw-tv-count" role="status" data-find-count="">
          {count ?? ""}
        </span>
        <button
          type="button"
          className="cw-focusable cw-button"
          data-action="wrap"
          aria-pressed={wrap}
          onClick={() => setWrap((w) => !w)}
        >
          <WrapText aria-hidden />
          <span>{t("cw_files_vw_wrap")}</span>
        </button>
      </form>
      <div ref={body} className={`cw-tv-body${wrap ? " is-wrap" : ""}`} style={{ ["--cw-gutter" as string]: `${gutter}ch` }}>
        {!rows.length ? <p className="cw-dim">{t("cw_files_vw_empty")}</p> : null}
        {win.start > 0 ? (
          <button
            type="button"
            className="cw-focusable cw-button cw-more"
            data-action="earlier-lines"
            onClick={() => setWin({ start: Math.max(0, win.start - ROW_WINDOW), end: win.end })}
          >
            <span>{t("cw_files_vw_earlier_lines", { n: win.start.toLocaleString(getLanguage()) })}</span>
          </button>
        ) : null}
        {blocks.slice(firstBlock, lastBlock).map((b, i) => {
          const k = firstBlock + i;
          return <Block key={k} rows={b} first={k * BLOCK_ROWS} marks={byBlock.get(k)} current={k === curBlock ? current : -1} />;
        })}
        {rows.length > win.end ? (
          <button
            type="button"
            className="cw-focusable cw-button cw-more"
            data-action="more-lines"
            onClick={() => setWin({ start: win.start, end: Math.min(rows.length, win.end + ROW_WINDOW) })}
          >
            <span>{t("cw_files_vw_more_lines", { n: (rows.length - win.end).toLocaleString(getLanguage()) })}</span>
          </button>
        ) : null}
      </div>
    </div>
  );
}
