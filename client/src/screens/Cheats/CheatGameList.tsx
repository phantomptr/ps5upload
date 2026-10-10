import { useState } from "react";
import { ChevronDown, Search } from "lucide-react";

import { GameIcon } from "../../components";
import {
  cheatFiltersActive,
  EMPTY_CHEAT_FILTERS,
  hasVersionMatch,
  type CheatGame,
  type CheatListFilters,
  type CheatScopeFilter,
  type CheatSection,
  type CheatStateFilter,
} from "../../lib/cheatGames";
import { hostOf } from "../../lib/addr";
import { useTr } from "../../state/lang";
import type { AppInfoDetails } from "../../api/ps5";

/** The left column: every game on the console, grouped by what you can do
 *  with it next — play with cheats, switch cheats on, download some. */
export function CheatGameList({
  host,
  sections,
  filters,
  onFilters,
  selectedId,
  onSelect,
  details,
}: {
  host: string;
  sections: CheatSection[];
  filters: CheatListFilters;
  onFilters: (f: CheatListFilters) => void;
  selectedId: string | null;
  onSelect: (titleId: string) => void;
  details: Map<string, AppInfoDetails | null>;
}) {
  const tr = useTr();
  // Games nobody has published cheats for are listed, but folded: they are
  // usually apps (YouTube, a browser, a loader), and a player scanning for
  // something to cheat in shouldn't have to read past them.
  const [showNone, setShowNone] = useState(false);
  const heading: Record<CheatSection["key"], string> = {
    playing: tr("cheats_section_playing", undefined, "Playing now"),
    ready: tr("cheats_section_ready", undefined, "Cheats ready"),
    available: tr("cheats_section_available", undefined, "Cheats to download"),
    none: tr("cheats_section_none", undefined, "No cheats published"),
  };
  const total = sections.reduce((n, s) => n + s.games.length, 0);
  const query = filters.query;
  const filtering = cheatFiltersActive(filters);
  const scopes: [CheatScopeFilter, string][] = [
    ["all", tr("cheats_filter_scope_all", undefined, "All games")],
    ["installed", tr("cheats_filter_scope_installed", undefined, "Installed here")],
    ["withCheats", tr("cheats_filter_scope_with", undefined, "With cheats")],
  ];
  const states: [CheatStateFilter, string][] = [
    ["all", tr("cheats_filter_state_all", undefined, "Any state")],
    ["playing", tr("cheats_filter_state_playing", undefined, "Playing now")],
    ["switchedOn", tr("cheats_filter_state_on", undefined, "Cheats switched on")],
    ["ready", tr("cheats_filter_state_ready", undefined, "Cheats ready")],
    ["toDownload", tr("cheats_filter_state_download", undefined, "Cheats to download")],
  ];

  return (
    <div className="flex min-w-0 flex-col gap-3">
      <label className="relative block">
        <Search
          size={14}
          className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-[var(--color-muted)]"
        />
        <input
          type="search"
          value={query}
          onChange={(e) => onFilters({ ...filters, query: e.target.value })}
          placeholder={tr("cheats_find_game", undefined, "Find a game")}
          aria-label={tr("cheats_find_game", undefined, "Find a game")}
          className="input"
          // `.input` is plain CSS and sets its own padding, which a utility
          // class cannot beat — without this the icon sits on the text.
          style={{ paddingLeft: "2rem" }}
        />
      </label>

      <div className="grid grid-cols-3 gap-1.5" role="group" aria-label={tr("cheats_filters", undefined, "Filters")}>
        <select
          className="input"
          value={filters.scope}
          onChange={(e) => onFilters({ ...filters, scope: e.target.value as CheatScopeFilter })}
          aria-label={tr("cheats_filter_scope", undefined, "Games")}
        >
          {scopes.map(([v, l]) => (
            <option key={v} value={v}>
              {l}
            </option>
          ))}
        </select>
        <select
          className="input"
          value={filters.state}
          onChange={(e) => onFilters({ ...filters, state: e.target.value as CheatStateFilter })}
          aria-label={tr("cheats_filter_state", undefined, "State")}
        >
          {states.map(([v, l]) => (
            <option key={v} value={v}>
              {l}
            </option>
          ))}
        </select>
        <select
          className="input"
          value={filters.format}
          onChange={(e) => onFilters({ ...filters, format: e.target.value })}
          aria-label={tr("cheats_filter_format", undefined, "Format")}
        >
          <option value="">{tr("cheats_filter_format_any", undefined, "Any format")}</option>
          {["json", "shn", "mc4"].map((f) => (
            <option key={f} value={f}>
              {f.toUpperCase()}
            </option>
          ))}
        </select>
      </div>
      {filtering && (
        <button
          type="button"
          onClick={() => onFilters(EMPTY_CHEAT_FILTERS)}
          className="self-start text-xs text-[var(--color-accent)] hover:underline"
        >
          {tr("cheats_filter_clear", undefined, "Clear filters")}
        </button>
      )}

      {total === 0 && (
        <p className="px-1 py-4 text-center text-sm text-[var(--color-muted)]">
          {filtering
            ? tr("cheats_no_game_match", undefined, "No game matches that search.")
            : tr("cheats_no_games", undefined, "No games found on this PS5.")}
        </p>
      )}

      {sections.map((section) => {
        const folded = section.key === "none" && !showNone && !filtering;
        return (
          <section key={section.key}>
            {section.key === "none" ? (
              <button
                type="button"
                onClick={() => setShowNone((v) => !v)}
                className="mb-1.5 flex w-full items-center gap-2 text-[11px] font-semibold uppercase tracking-wide text-[var(--color-muted)] hover:text-[var(--color-text)]"
                aria-expanded={!folded}
              >
                <span>{heading[section.key]}</span>
                <span className="rounded-full bg-[var(--color-surface-3)] px-1.5 font-mono text-[10px] tabular-nums">
                  {section.games.length}
                </span>
                <ChevronDown
                  size={12}
                  className={`ml-auto transition-transform ${folded ? "" : "rotate-180"}`}
                />
              </button>
            ) : (
              <div className="mb-1.5 flex items-center gap-2 text-[11px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
                <span>{heading[section.key]}</span>
                <span className="rounded-full bg-[var(--color-surface-3)] px-1.5 font-mono text-[10px] tabular-nums">
                  {section.games.length}
                </span>
              </div>
            )}
            {!folded && (
              <ul className="grid gap-1">
                {section.games.map((g) => (
                  <GameRow
                    key={g.titleId}
                    host={host}
                    game={g}
                    selected={g.titleId === selectedId}
                    version={details.get(g.titleId)?.version ?? null}
                    onSelect={() => onSelect(g.titleId)}
                  />
                ))}
              </ul>
            )}
          </section>
        );
      })}
    </div>
  );
}

function GameRow({
  host,
  game,
  selected,
  version,
  onSelect,
}: {
  host: string;
  game: CheatGame;
  selected: boolean;
  version: string | null;
  onSelect: () => void;
}) {
  const tr = useTr();
  const n = game.available.length;
  const match = version ? hasVersionMatch(game.available, version) : false;
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        aria-current={selected ? "true" : undefined}
        className={`flex w-full items-center gap-3 rounded-[var(--radius-field)] border px-2.5 py-2 text-left transition-colors ${
          selected
            ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
            : "border-transparent hover:border-[var(--color-border)] hover:bg-[var(--color-surface-2)]"
        }`}
      >
        <GameIcon host={hostOf(host)} titleId={game.titleId} size={40} />
        <span className="min-w-0 flex-1">
          <span className="line-clamp-2 text-sm font-medium [overflow-wrap:anywhere]">
            {game.name}
          </span>
          <span className="mt-0.5 block truncate font-mono text-[11px] text-[var(--color-muted)]">
            {game.titleId}
            {version ? ` · v${version}` : ""}
          </span>
        </span>
        <span className="shrink-0 text-right text-[11px] font-medium">
          {game.running ? (
            <span className="inline-flex items-center gap-1 text-[var(--color-good)]">
              <span className="h-1.5 w-1.5 rounded-full bg-[var(--color-good)]" />
              {tr("cheats_badge_playing", undefined, "Playing")}
            </span>
          ) : game.downloaded ? (
            <span className="text-[var(--color-accent)]">
              {game.enabledCount > 0
                ? tr("cheats_badge_on", { n: game.enabledCount }, `${game.enabledCount} on`)
                : tr("cheats_badge_ready", undefined, "Ready")}
            </span>
          ) : n > 0 ? (
            <span className={match ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"}>
              {tr("cheats_badge_count", { n }, `${n} available`)}
              {match && (
                <span className="block text-[10px] font-normal">
                  {tr("cheats_badge_your_version", undefined, "for your version")}
                </span>
              )}
            </span>
          ) : null}
        </span>
      </button>
    </li>
  );
}
