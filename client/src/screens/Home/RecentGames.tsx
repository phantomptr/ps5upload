import { useEffect, useMemo, useState } from "react";
import { Link } from "react-router";
import { ArrowRight } from "lucide-react";
import { useShallow } from "zustand/react/shallow";

import { CoverCaption, CoverFrame, CoverTag, MetaDot, PlatformTag } from "../../components/GameIconFrame";
import { formatCollectionBytes } from "../../lib/collectionView";
import { gamePath } from "../../lib/gamePage";
import { hostOf } from "../../lib/addr";
import { collectionGames, loadCollection, loadConsoleStates, useCollectionStore } from "../../state/collection";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { CollectionCover } from "../Collection/CollectionCover";
import { recentGames, type RecentFilter, type StatesByHost } from "./recentGamesFilter";

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
            to="/collection"
            className="inline-flex shrink-0 items-center gap-1 text-xs font-semibold text-[var(--color-accent)] hover:underline"
          >
            {tr("home_recent_games_all_link", undefined, "Collection")}
            <ArrowRight size={13} />
          </Link>
        </div>
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
      </div>
      {shown.length === 0 ? (
        <p className="surface-panel !rounded-[var(--radius-card)] px-5 py-4 text-sm text-[var(--color-muted)]">
          {tr("home_recent_games_none", undefined, "No games match.")}
        </p>
      ) : (
        // One row that scrolls sideways on a narrow window rather than wrapping
        // into a wall of covers on Home.
        <ul className="-mx-2 -mb-2 flex snap-x gap-4 overflow-x-auto px-2 pt-1 pb-4 [scrollbar-width:thin]">
          {shown.map((g) => {
            const installedHere = !!connectedStates?.[g.game_id]?.installed;
            return (
              <li
                key={g.game_id}
                // Three to a row at the column's width, as in the reference; the rest scroll.
                className="w-[9.5rem] shrink-0 snap-start sm:w-[calc((100%-2rem)/3)] sm:min-w-[10rem]"
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
                      <>
                        <span className="truncate font-mono">{g.game_id}</span>
                        <MetaDot />
                        <span className="shrink-0 tabular-nums">
                          {formatCollectionBytes(g.total_size_bytes)}
                        </span>
                      </>
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
