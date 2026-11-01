import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import { ArrowRight, ChevronLeft, ChevronRight } from "lucide-react";
import { useShallow } from "zustand/react/shallow";

import { CoverCaption, CoverFrame, CoverTag, PlatformTag } from "../../components/GameIconFrame";
import { formatCollectionBytes } from "../../lib/collectionView";
import { gamePath } from "../../lib/gamePage";
import { hostOf } from "../../lib/addr";
import { collectionGames, loadCollection, loadConsoleStates, useCollectionStore } from "../../state/collection";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { CollectionCover } from "../Collection/CollectionCover";
import { recentGames, rowEdges, type RecentFilter, type StatesByHost } from "./recentGamesFilter";

const filterKey = (f: RecentFilter) => (typeof f === "string" ? f : `host:${f.host}`);

/**
 * The newest games in the Collection, framed like the reference's
 * "Recommended" row, with chips to show what each console has. Nothing at all
 * when the Collection is empty: Home should not advertise a feature with an
 * empty shelf.
 */
export function RecentGames() {
  const tr = useTr();
  const { library, consoleByHost } = useCollectionStore(
    useShallow((s) => ({ library: s.library, consoleByHost: s.consoleByHost })),
  );
  const host = useConnectionStore((s) => s.host?.trim() ?? "");
  const helperUp = useConnectionStore((s) => s.payloadStatus === "up");
  const profiles = useRosterStore((s) => s.profiles);
  const [filter, setFilter] = useState<RecentFilter>("all");

  // The Collection is kept in a store; load it once if no screen has yet.
  useEffect(() => {
    if (!useCollectionStore.getState().library) void loadCollection();
  }, []);
  // What the connected console has, once per session (the Collection screen
  // reads it again after every scan).
  const generatedAt = library?.generated_at;
  useEffect(() => {
    if (!host || !helperUp || !generatedAt) return;
    if (useCollectionStore.getState().consoleByHost[host]) return;
    void loadConsoleStates(host);
  }, [host, helperUp, generatedAt]);

  const games = useMemo(() => collectionGames(library), [library]);
  // Only consoles with a read: the others can't answer "is it there".
  const consoles = useMemo(
    () =>
      profiles
        .map((p) => ({ host: p.host.trim(), name: p.name || hostOf(p.host) }))
        .filter((c) => {
          const e = consoleByHost[c.host];
          return !!e && !e.error && Object.keys(e.states).length > 0;
        }),
    [profiles, consoleByHost],
  );
  const states: StatesByHost = useMemo(
    () => Object.fromEntries(consoles.map((c) => [c.host, consoleByHost[c.host]?.states])),
    [consoles, consoleByHost],
  );
  // A chip whose console went away falls back to everything.
  const active: RecentFilter =
    typeof filter === "object" && !consoles.some((c) => c.host === filter.host) ? "all" : filter;
  const shown = useMemo(() => recentGames(games, active, states), [games, active, states]);

  // Previous/next arrows for a mouse: shown only while the row overflows, and
  // each is off when there is nothing more that way. Touch swipes the row
  // instead (CSS hides the arrows there).
  const rowRef = useRef<HTMLUListElement>(null);
  const [edges, setEdges] = useState({ prev: false, next: false });
  const measure = useCallback(() => {
    const el = rowRef.current;
    if (!el) return;
    const e = rowEdges(el.scrollLeft, el.scrollWidth, el.clientWidth);
    setEdges((p) => (p.prev === e.prev && p.next === e.next ? p : e));
  }, []);
  useEffect(() => {
    const el = rowRef.current;
    if (!el) {
      setEdges({ prev: false, next: false });
      return;
    }
    measure();
    el.addEventListener("scroll", measure, { passive: true });
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    ro?.observe(el);
    return () => {
      el.removeEventListener("scroll", measure);
      ro?.disconnect();
    };
  }, [measure, shown]);
  const page = (towardsEnd: boolean) => {
    const el = rowRef.current;
    if (!el) return;
    // About a row's width at a time, keeping a card of overlap; the end is
    // leftwards in a right-to-left layout.
    const rtl = getComputedStyle(el).direction === "rtl";
    const step = Math.max(el.clientWidth * 0.85, 120) * (towardsEnd ? 1 : -1) * (rtl ? -1 : 1);
    const still = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    el.scrollBy({ left: step, behavior: still ? "auto" : "smooth" });
  };
  const overflowing = shown.length > 0 && (edges.prev || edges.next);

  if (games.length === 0) return null;

  const chips: Array<{ f: RecentFilter; label: string }> = [
    { f: "all", label: tr("home_recent_games_all", undefined, "Newest") },
    ...consoles.map((c) => ({
      f: { host: c.host } as RecentFilter,
      label: tr("home_recent_games_on", { name: c.name }, "On {name}"),
    })),
    ...(consoles.length > 0
      ? [{ f: "missing" as RecentFilter, label: tr("home_recent_games_missing", undefined, "Not installed") }]
      : []),
  ];
  const connectedStates = host ? consoleByHost[host]?.states : undefined;

  return (
    <section aria-labelledby="home-recent-games" data-testid="home-recent-games">
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div className="flex min-w-0 items-baseline gap-3">
          <h2
            id="home-recent-games"
            className="text-[1.125rem] font-semibold tracking-[-0.01em]"
          >
            {tr("home_recent_games", undefined, "Recent games")}
          </h2>
          <Link
            to="/games?tab=all"
            className="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-[var(--color-accent)] hover:underline"
          >
            {tr("home_recent_games_all_link", undefined, "Collection")}
            <ArrowRight size={13} className="rtl:rotate-180" />
          </Link>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {chips.length > 1 && (
            <div
              role="group"
              aria-label={tr("home_recent_games_filter", undefined, "Show games")}
              className="flex flex-wrap items-center gap-2"
            >
              {chips.map(({ f, label }) => (
                <button
                  key={filterKey(f)}
                  type="button"
                  aria-pressed={filterKey(active) === filterKey(f)}
                  onClick={() => setFilter(f)}
                  className="chip min-h-9 px-4 text-xs font-medium"
                >
                  {label}
                </button>
              ))}
            </div>
          )}
          {overflowing && (
            // For a mouse and keyboard only: a touch screen swipes the row.
            <div
              data-testid="home-recent-games-arrows"
              className="hidden items-center gap-1.5 [@media(hover:hover)_and_(pointer:fine)]:flex"
            >
              <button
                type="button"
                onClick={() => page(false)}
                disabled={!edges.prev}
                aria-label={tr("home_recent_games_prev", undefined, "Previous games")}
                title={tr("home_recent_games_prev", undefined, "Previous games")}
                className="chip h-9 w-9 justify-center disabled:cursor-default disabled:opacity-40"
              >
                <ChevronLeft size={16} className="rtl:rotate-180" />
              </button>
              <button
                type="button"
                onClick={() => page(true)}
                disabled={!edges.next}
                aria-label={tr("home_recent_games_next", undefined, "More games")}
                title={tr("home_recent_games_next", undefined, "More games")}
                className="chip h-9 w-9 justify-center disabled:cursor-default disabled:opacity-40"
              >
                <ChevronRight size={16} className="rtl:rotate-180" />
              </button>
            </div>
          )}
        </div>
      </div>
      {shown.length === 0 ? (
        <p className="surface-panel !rounded-[var(--radius-card)] px-5 py-4 text-sm text-[var(--color-muted)]">
          {tr("home_recent_games_none", undefined, "No games match.")}
        </p>
      ) : (
        // One row of covers about the size of the console's own home screen,
        // scrolling sideways when there are more than fit rather than wrapping
        // into a wall of covers on Home.
        <ul
          ref={rowRef}
          data-testid="home-recent-games-row"
          className="-mx-2 -mb-2 flex snap-x snap-mandatory scroll-px-2 gap-3 overflow-x-auto px-2 pt-1 pb-4 [scrollbar-width:thin]"
        >
          {shown.map((g) => {
            const installedHere = !!connectedStates?.[g.game_id]?.installed;
            return (
              <li
                key={g.game_id}
                // A fixed width, so a wide window shows more games, not bigger ones.
                className="w-[7.75rem] shrink-0 snap-start sm:w-[8.5rem]"
              >
                <Link to={gamePath(g.game_id)} className="group block rounded-[var(--radius-card)]">
                  <CoverFrame
                    interactive
                    overlay={
                      <>
                        <span className="absolute left-2 top-2">
                          <PlatformTag platform={g.platform} />
                        </span>
                        {installedHere && (
                          <span className="absolute bottom-2 left-2">
                            <CoverTag tone="good">
                              {tr("game_state_installed", undefined, "Installed")}
                            </CoverTag>
                          </span>
                        )}
                      </>
                    }
                  >
                    <CollectionCover game={g} className="h-full" />
                  </CoverFrame>
                  <CoverCaption
                    title={g.title}
                    titleAttr={g.title}
                    meta={
                      // The id and the size, the size on its own line when the
                      // narrow card can't hold both: a cut-off id is no use.
                      <span className="flex min-w-0 flex-wrap items-center gap-x-2">
                        <span className="truncate font-mono">{g.game_id}</span>
                        <span className="shrink-0 tabular-nums">
                          {formatCollectionBytes(g.total_size_bytes)}
                        </span>
                      </span>
                    }
                  />
                </Link>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
