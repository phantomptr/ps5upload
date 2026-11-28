import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { RefreshCw, Search, X } from "lucide-react";

import { fetchHubPreview, fetchRooms, hubMissText, type HubMiss, type RoomsSource } from "../lib/room/hubRooms";
import { LANGUAGE_LIST } from "../lib/languageNames";
import { playerNameProblem, nameProblemText } from "../lib/room/names";
import { hiddenRooms, historyFor, hideRoom, playedHosts } from "../lib/room/played";
import type { Region } from "../lib/room/regions";
import {
  cardAction,
  DEFAULT_QUERY,
  expectedFor,
  filtered,
  hiddenBy,
  olderVersion,
  refineOrder,
  ROOM_SORTS,
  type RoomCard,
  type RoomList,
  type RoomQuery,
  type RoomSort,
} from "../lib/room/rooms";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useTr } from "../state/lang";
import { RoomCardView, cardActionText } from "./RoomCardView";
import { RoomDetails } from "./RoomDetails";

type Tr = ReturnType<typeof useTr>;

const SORT_KEY = "ps5upload.rooms.sort";

export function sortLabel(tr: Tr, s: RoomSort): string {
  switch (s) {
    case "best":
      return tr("rooms_sort_best", undefined, "Best for you");
    case "latency":
      return tr("rooms_sort_latency", undefined, "Lowest latency");
    case "game":
      return tr("rooms_sort_game", undefined, "Game");
    case "players":
      return tr("rooms_sort_players", undefined, "Most players");
    case "space":
      return tr("rooms_sort_space", undefined, "Has space");
    case "newest":
      return tr("rooms_sort_newest", undefined, "Newest");
  }
}

function savedSort(): RoomSort {
  const v = safeGetItem(SORT_KEY);
  return (ROOM_SORTS as readonly string[]).includes(v ?? "") ? (v as RoomSort) : "best";
}

/** What the knock form hands over: the name to show and, for a locked room, the password. */
export interface KnockInput {
  name: string;
  password: string | null;
}

/**
 * The Rooms browser's public list (design 7.0), shared by the app (Play › Play Room › Rooms)
 * and the website's Rooms page: sort, filters, cards, Details, and the knock form (name, then the
 * password if any). Friends' rooms, Shared with you and Nearby come first from the caller
 * (`groups`), never paged away. Joining is the caller's (the same join flow as an invite).
 */
export function RoomsBrowserCore({
  source,
  me,
  version,
  groups,
  cover,
  dark = false,
  defaultName,
  onJoin,
  onCode,
  onCreate,
  ensureNotice,
  testId = "rooms",
}: {
  source: RoomsSource | null;
  me: Region | null;
  /** This build's version ("A newer PS5Upload is available"). */
  version: string;
  groups?: ReactNode;
  cover?: (c: RoomCard) => ReactNode;
  dark?: boolean;
  defaultName: string;
  onJoin: (card: RoomCard, k: KnockInput) => void;
  onCode?: () => void;
  onCreate?: () => void;
  /** The responsibility notice before anything from Rooms on this device. */
  ensureNotice: () => Promise<boolean>;
  testId?: string;
}) {
  const tr = useTr();
  const [q, setQ] = useState<RoomQuery>(() => ({ ...DEFAULT_QUERY, sort: savedSort(), me }));
  const [list, setList] = useState<RoomList | HubMiss | null>(null);
  const [pages, setPages] = useState<RoomCard[]>([]);
  const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState<RoomCard | null>(null);
  const [knock, setKnock] = useState<RoomCard | null>(null);
  const [hidden, setHidden] = useState(() => hiddenRooms());
  const [focus, setFocus] = useState<string | null>(null);
  const seq = useRef(0);
  const played = useMemo(() => playedHosts(), []);

  const load = useCallback(
    async (query: RoomQuery, more: boolean) => {
      if (!source) return;
      const n = ++seq.current;
      setBusy(true);
      const r = await fetchRooms(source, query);
      if (n !== seq.current) return;
      setBusy(false);
      setList(r);
      if (typeof r === "object") setPages((old) => (more ? [...old, ...r.rooms.filter((c) => !old.some((o) => o.room === c.room))] : r.rooms));
    },
    [source],
  );
  useEffect(() => {
    void load(q, q.page > 0);
  }, [q, load]);
  const set = (p: Partial<RoomQuery>) => setQ((old) => ({ ...old, ...p, page: 0 }));

  // The time measurements are judged against: when this list was read.
  const [now, setNow] = useState(0);
  useEffect(() => setNow(Date.now()), [pages]);
  const exp = useCallback((c: RoomCard) => expectedFor(c, me, historyFor(c.tag, played), now), [me, played, now]);
  const cards = refineOrder(
    pages.filter((c) => !hiddenBy(c, hidden)),
    q.sort,
    exp,
    focus != null,
  );
  const meta = typeof list === "object" && list ? list : null;
  const minProto = meta?.proto.min;
  const newer = meta?.appLatest && olderVersion(version, meta.appLatest) ? meta.appLatest : null;

  const ask = async (c: RoomCard) => {
    if (!(await ensureNotice())) return;
    setDetails(null);
    setKnock(c);
  };

  const muted = dark ? "text-white/60" : "text-[var(--color-muted)]";
  const field = dark
    ? "rounded-lg border border-white/15 bg-black px-3 py-1.5 text-sm text-white outline-none focus:border-white/50"
    : "rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm";
  const chip = (on: boolean) =>
    `rounded-full px-2.5 py-1 text-xs ${
      on
        ? dark
          ? "bg-white text-black"
          : "bg-[var(--color-accent)] text-[var(--color-on-accent,#fff)]"
        : dark
          ? "bg-white/10"
          : "border border-[var(--color-border)]"
    }`;

  return (
    <section className="space-y-3" data-testid={testId}>
      <div className="flex flex-wrap items-center gap-2">
        <label className="relative min-w-[10rem] flex-1">
          <Search size={14} className={`absolute start-2.5 top-1/2 -translate-y-1/2 ${muted}`} aria-hidden />
          <input
            value={q.game}
            onChange={(e) => set({ game: e.target.value })}
            placeholder={tr("rooms_search_game", undefined, "Search a game…")}
            aria-label={tr("rooms_search_game", undefined, "Search a game…")}
            className={`${field} w-full ps-8`}
            data-testid={`${testId}-search`}
          />
        </label>
        <select
          value={q.sort}
          onChange={(e) => {
            safeSetItem(SORT_KEY, e.target.value);
            set({ sort: e.target.value as RoomSort });
          }}
          aria-label={tr("rooms_sort", undefined, "Sort")}
          className={field}
          data-testid={`${testId}-sort`}
        >
          {ROOM_SORTS.map((s) => (
            <option key={s} value={s}>
              {sortLabel(tr, s)}
            </option>
          ))}
        </select>
        <button type="button" className={chip(false)} onClick={() => void load(q, false)} aria-label={tr("rooms_refresh", undefined, "Refresh")} data-testid={`${testId}-refresh`}>
          <RefreshCw size={12} className={busy ? "animate-spin" : ""} />
        </button>
        {onCode && (
          <button type="button" className={chip(false)} onClick={onCode} data-testid={`${testId}-code`}>
            {tr("rooms_code_link", undefined, "Join with a code/link")}
          </button>
        )}
      </div>
      <div className="flex flex-wrap items-center gap-1.5" data-testid={`${testId}-filters`}>
        <button type="button" className={chip(q.free)} aria-pressed={q.free} onClick={() => set({ free: !q.free })}>
          {tr("rooms_sort_space", undefined, "Has space")}
        </button>
        <button type="button" className={chip(q.nopw)} aria-pressed={q.nopw} onClick={() => set({ nopw: !q.nopw })}>
          {tr("rooms_no_password", undefined, "No password")}
        </button>
        <button type="button" className={chip(q.compatible)} aria-pressed={q.compatible} onClick={() => set({ compatible: !q.compatible })}>
          {tr("rooms_compatible", undefined, "Works with my version")}
        </button>
        <button type="button" className={chip(q.near)} aria-pressed={q.near} disabled={!me} onClick={() => set({ near: !q.near })}>
          {tr("rooms_near_me", undefined, "Near me")}
        </button>
        <select
          value={q.lang ?? ""}
          onChange={(e) => set({ lang: e.target.value || null })}
          aria-label={tr("room_language", undefined, "Language")}
          className={`${field} py-1 text-xs`}
          data-testid={`${testId}-lang`}
        >
          <option value="">{tr("rooms_any_language", undefined, "Any language")}</option>
          {LANGUAGE_LIST.map((l) => (
            <option key={l.code} value={l.code}>
              {l.label}
            </option>
          ))}
        </select>
        {filtered(q) && (
          <button type="button" className={`text-xs underline ${muted}`} onClick={() => setQ({ ...DEFAULT_QUERY, sort: q.sort, me })} data-testid={`${testId}-clear`}>
            {tr("rooms_clear_filters", undefined, "Clear filters")}
          </button>
        )}
      </div>
      {newer && (
        <p className={`text-xs ${muted}`} data-testid={`${testId}-newer`}>
          {tr("rooms_newer_app", undefined, "A newer PS5Upload is available")} ({newer})
        </p>
      )}

      {groups}

      <h3 className="text-sm font-semibold">{tr("rooms_public", undefined, "Public rooms")}</h3>
      {list === null && <p className={`text-sm ${muted}`}>{tr("rooms_loading", undefined, "Looking for rooms…")}</p>}
      {typeof list === "string" && (
        <div className="space-y-2" data-testid={`${testId}-miss`} data-why={list}>
          <p className={`text-sm ${muted}`}>{hubMissText(tr, list)}</p>
          {list === "unreachable" && (
            <button type="button" className={chip(false)} onClick={() => void load(q, false)}>
              {tr("join_retry", undefined, "Try again")}
            </button>
          )}
        </div>
      )}
      {meta && cards.length === 0 && (
        <div className="space-y-2" data-testid={`${testId}-empty`}>
          {filtered(q) ? (
            <p className={`text-sm ${muted}`}>
              {tr("rooms_none_match", undefined, "No rooms match.")}{" "}
              <button type="button" className="underline" onClick={() => setQ({ ...DEFAULT_QUERY, sort: q.sort, me })}>
                {tr("rooms_clear_filters", undefined, "Clear filters")}
              </button>
            </p>
          ) : (
            <>
              <p className={`text-sm ${muted}`}>{tr("rooms_none", undefined, "No public rooms right now.")}</p>
              <div className="flex flex-wrap gap-2">
                {onCreate && (
                  <button type="button" className={chip(true)} onClick={onCreate}>
                    {tr("room_create", undefined, "Create a Play Room")}
                  </button>
                )}
                {onCode && (
                  <button type="button" className={chip(false)} onClick={onCode}>
                    {tr("rooms_got_invite", undefined, "Got an invite? Join with a code/link")}
                  </button>
                )}
              </div>
            </>
          )}
        </div>
      )}
      {cards.length > 0 && (
        <ul className="space-y-2" data-testid={`${testId}-list`}>
          {cards.map((c) => {
            const action = cardAction(c, { minProto });
            return (
              <div key={c.room} onMouseEnter={() => setFocus(c.room)} onMouseLeave={() => setFocus(null)} onFocus={() => setFocus(c.room)} onBlur={() => setFocus(null)}>
                <RoomCardView
                  card={c}
                  expected={exp(c)}
                  action={action}
                  dark={dark}
                  cover={cover?.(c)}
                  onAction={() => void ask(c)}
                  onDetails={() => setDetails(c)}
                  extra={
                    <button
                      type="button"
                      className={`text-[11px] underline ${muted}`}
                      onClick={() => {
                        hideRoom(`tag:${c.tag}`);
                        setHidden(hiddenRooms());
                      }}
                      data-testid="room-card-hide"
                    >
                      {tr("rooms_hide_host", undefined, "Hide this host")}
                    </button>
                  }
                />
              </div>
            );
          })}
        </ul>
      )}
      {meta?.next != null && (
        <button type="button" className={chip(false)} onClick={() => setQ((o) => ({ ...o, page: meta.next ?? 0 }))} data-testid={`${testId}-more`}>
          {tr("rooms_more", undefined, "More rooms")}
        </button>
      )}

      {details && source && (
        <Sheet dark={dark} onClose={() => setDetails(null)}>
          <RoomDetails
            card={details}
            expected={exp(details)}
            dark={dark}
            load={() => fetchHubPreview(source, details.room)}
            onAsk={() => void ask(details)}
            askLabel={cardActionText(tr, cardAction(details, { minProto }), details.proto)}
          />
        </Sheet>
      )}
      {knock && (
        <Sheet dark={dark} onClose={() => setKnock(null)}>
          <KnockForm
            card={knock}
            dark={dark}
            defaultName={defaultName}
            onGo={(k) => {
              setKnock(null);
              onJoin(knock, k);
            }}
          />
        </Sheet>
      )}
    </section>
  );
}

/** A sheet over the page (bottom on a phone, centred on a computer). */
export function Sheet({ children, onClose, dark }: { children: ReactNode; onClose: () => void; dark: boolean }) {
  const tr = useTr();
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);
  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center bg-black/60 sm:items-center" onClick={onClose} role="presentation">
      <div
        role="dialog"
        aria-modal="true"
        className={`relative max-h-[90dvh] w-full max-w-md overflow-y-auto rounded-t-2xl p-5 sm:rounded-2xl ${
          dark ? "bg-neutral-900 text-white" : "bg-[var(--color-surface-raised)]"
        }`}
        onClick={(e) => e.stopPropagation()}
      >
        <button type="button" onClick={onClose} aria-label={tr("rp_phone_close", undefined, "Close")} className="absolute end-3 top-3 rounded p-1 opacity-70 hover:opacity-100">
          <X size={16} />
        </button>
        {children}
      </div>
    </div>
  );
}

/** "Ask to join <room>": your name, the password if the room has one (design 7.1). */
export function KnockForm({
  card,
  dark,
  defaultName,
  onGo,
}: {
  card: Pick<RoomCard, "name" | "pw">;
  dark: boolean;
  defaultName: string;
  onGo: (k: KnockInput) => void;
}) {
  const tr = useTr();
  const [name, setName] = useState(defaultName);
  const [pw, setPw] = useState("");
  const problem = playerNameProblem(name);
  const ok = !problem && (!card.pw || pw.length >= 4);
  const field = dark
    ? "block w-full rounded-lg border border-white/15 bg-black px-3 py-2 text-base text-white outline-none focus:border-white/50"
    : "block w-full rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm";
  return (
    <form
      className="space-y-3"
      onSubmit={(e) => {
        e.preventDefault();
        if (ok) onGo({ name: name.trim(), password: card.pw ? pw : null });
      }}
      data-testid="room-knock"
    >
      <h2 className="text-base font-semibold">{tr("rooms_ask_title", { room: card.name }, `Ask to join ${card.name}`)}</h2>
      <label className="block space-y-1">
        <span className="text-sm font-medium">{tr("join_name_label", undefined, "Your name")}</span>
        <input value={name} maxLength={24} onChange={(e) => setName(e.target.value)} autoComplete="nickname" className={field} data-testid="room-knock-name" />
        {name.trim() && problem && <span className="block text-xs text-amber-500">{nameProblemText(tr, problem, false)}</span>}
      </label>
      {card.pw && (
        <label className="block space-y-1">
          <span className="text-sm font-medium">{tr("room_password", undefined, "Password")}</span>
          <input type="password" value={pw} maxLength={64} onChange={(e) => setPw(e.target.value)} autoComplete="current-password" className={field} data-testid="room-knock-password" />
        </label>
      )}
      <button
        type="submit"
        disabled={!ok}
        className={
          dark
            ? "w-full rounded-full bg-white py-2.5 text-sm font-semibold text-black disabled:opacity-40"
            : "w-full rounded-full bg-[var(--color-accent)] py-2 text-sm font-semibold text-[var(--color-on-accent,#fff)] disabled:opacity-40"
        }
        data-testid="room-knock-go"
      >
        {tr("rooms_ask_go", undefined, "Ask")}
      </button>
    </form>
  );
}
