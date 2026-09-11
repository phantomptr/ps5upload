import { useCallback, useEffect, useMemo, useState } from "react";
import {
  cheatFilterOptions,
  filterCheatEntries,
  NO_CHEAT_FILTERS,
  type CheatFilters,
} from "../../lib/cheatBrowse";
import {
  Search,
  Download,
  X,
  CheckCircle2,
  ExternalLink,
  Package,
} from "lucide-react";
import { Button, ErrorCard, Card, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import {
  cheatsReposList,
  cheatsReposSearch,
  cheatsReposDownload,
  type CheatRepo,
  type CheatRepoEntry,
} from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";
import { appsInstalled, type InstalledTitle } from "../../api/ps5";

interface RepoBrowserProps {
  addr: string;
  onDownloaded: () => void;
  onClose: () => void;
}


/** What to show as the row's name.
 *
 *  Not every source publishes game titles. One of the built-in sources
 *  has no index file at all and is enumerated by listing the repository,
 *  which yields filenames but no titles -- so `game_title` is empty for
 *  those rows. Falling through to the raw entry would render a blank
 *  line, so derive the title id from the filename instead: it is what
 *  the user searched by, and a title id beats nothing. */
/** The title id a cheat file is named after, or null. */
function titleIdOf(filename: string): string | null {
  const m = filename.match(/^([A-Za-z]{4}\d{5})/);
  return m ? m[1].toUpperCase() : null;
}

/** What to show as the row's name.
 *
 *  Sources, best first:
 *   1. the repo's own game title, when it publishes one
 *   2. the name your console reports for that game — one of the built-in
 *      sources has no index and is enumerated by listing the repository,
 *      which yields filenames but no titles, so without this every row
 *      from it read as a bare id
 *   3. the title id, which at least identifies the game
 *
 *  Falling through to the raw filename is the last resort. */
function displayTitle(
  e: { game_title: string; filename: string },
  namesByTitleId: Map<string, string>,
): string {
  const t = e.game_title.trim();
  if (t) return t;
  const id = titleIdOf(e.filename);
  if (id) {
    const known = namesByTitleId.get(id);
    if (known) return known;
    return id;
  }
  return e.filename;
}

export function RepoBrowser({ addr, onDownloaded, onClose }: RepoBrowserProps) {
  const tr = useTr();
  const [repos, setRepos] = useState<CheatRepo[]>([]);
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState<CheatRepoEntry[]>([]);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [downloaded, setDownloaded] = useState<Set<string>>(new Set());
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [games, setGames] = useState<InstalledTitle[]>([]);
  const [loadingGames, setLoadingGames] = useState(false);
  const [filters, setFilters] = useState<CheatFilters>(NO_CHEAT_FILTERS);

  /* Names for the games on this console, keyed by title id. Cheat files
   * are named by title id, so this turns "PPSA01234" into the game's
   * actual name for anything the user has installed. */
  const namesByTitleId = useMemo(() => {
    const m = new Map<string, string>();
    for (const g of games) {
      if (g.titleId && g.titleName) m.set(g.titleId.toUpperCase(), g.titleName);
    }
    return m;
  }, [games]);

  const installedTitleIds = useMemo(
    () => new Set(games.map((g) => (g.titleId || "").toUpperCase()).filter(Boolean)),
    [games],
  );
  // Options come from the rows actually fetched, so a repo that starts
  // publishing a new format or version needs no code change here.
  const options = useMemo(() => cheatFilterOptions(entries), [entries]);
  const visible = useMemo(
    () => filterCheatEntries(entries, filters, installedTitleIds),
    [entries, filters, installedTitleIds],
  );

  useEffect(() => {
    void (async () => {
      try {
        const r = await cheatsReposList();
        setRepos(r);
      } catch (e) {
        setError(humanizePs5Error(String(e)));
      }
    })();
  }, []);

  // The installed games, so the common case is "click your game"
  // rather than "know its title id or guess its spelling".
  useEffect(() => {
    void (async () => {
      if (!addr) return;
      setLoadingGames(true);
      try {
        const r = await appsInstalled(addr);
        setGames(r.titles.filter((t) => !t.system));
      } catch {
        // Non-fatal: search by hand still works, so a failed listing
        // degrades instead of blocking the dialog.
        setGames([]);
      } finally {
        setLoadingGames(false);
      }
    })();
  }, [addr]);

  /* Search for one installed game by its title id.
   *
   * Cheat files are named after the title id, so this is the reliable
   * way to find them -- far better than typing a game's name and hoping
   * the repo spells it the same way. Takes the id explicitly so a click
   * searches immediately instead of waiting a render for setQuery. */
  const searchFor = useCallback(async (q: string) => {
    setQuery(q);
    setSearching(true);
    setError(null);
    setDownloadError(null);
    try {
      const r = await cheatsReposSearch(q);
      setEntries(r.entries ?? []);
      if (r.error) setError(r.error);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setSearching(false);
    }
  }, []);

  const handleSearch = useCallback(async () => {
    setSearching(true);
    setError(null);
    setDownloadError(null);
    try {
      const r = await cheatsReposSearch(query);
      setEntries(r.entries ?? []);
      if (r.error) setError(r.error);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setSearching(false);
    }
  }, [query]);

  const handleDownload = async (entry: CheatRepoEntry) => {
    setDownloading(entry.filename);
    setDownloadError(null);
    try {
      const titleId = entry.filename.split(/[._]/)[0] || entry.filename;
      const r = await cheatsReposDownload(
        // Ask the repo the entry actually came from. This was hardcoded
        // to "etahen", so anything found only in one of the other repos
        // was fetched from the wrong place and could not be found.
        entry.repo_id,
        entry.filename,
        titleId,
        addr,
      );
      if (r.ok) {
        setDownloaded((prev) => new Set(prev).add(entry.filename));
        onDownloaded();
      } else {
        setDownloadError(r.error || "Download failed");
      }
    } catch (e) {
      setDownloadError(humanizePs5Error(String(e)));
    } finally {
      setDownloading(null);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4">
      <div className="flex max-h-[85dvh] w-full max-w-2xl flex-col rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] shadow-xl">
        <div className="flex items-center justify-between border-b border-[var(--color-border)] px-4 py-3">
          <div className="flex items-center gap-2">
            <Package size={18} />
            <h2 className="text-sm font-semibold">
              {tr(
                "cheats_download_title",
                undefined,
                "Download Community Cheats",
              )}
            </h2>
          </div>
          <button onClick={onClose} className="text-[var(--color-muted)] hover:text-[var(--color-text)]">
            <X size={18} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-3">
          {repos.length > 0 && (
            <div className="flex flex-wrap gap-2">
              {repos.map((r) => (
                <a
                  key={r.id}
                  href={`https://github.com/${r.name}`}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="badge flex items-center gap-1 hover:border-[var(--color-accent)]"
                >
                  <ExternalLink size={10} />
                  {r.name}
                </a>
              ))}
            </div>
          )}

          {/* Your installed games. Cheat files are named by title id, so
              clicking a game searches by the thing the repos actually
              key on -- no need to know or guess an id. */}
          {games.length > 0 && (
            <div className="mb-3">
              <div className="mb-1.5 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
                {tr("cheats_your_games", undefined, "Your games")}
              </div>
              <div className="max-h-48 overflow-auto rounded border border-[var(--color-border)]">
                {games.map((g) => (
                  <button
                    key={g.titleId}
                    type="button"
                    onClick={() => void searchFor(g.titleId)}
                    className="flex w-full items-center justify-between gap-3 border-b border-[var(--color-border)] px-3 py-2 text-left last:border-b-0 hover:bg-[var(--color-surface-2)]"
                  >
                    <span className="min-w-0 flex-1 truncate text-sm">
                      {g.titleName || g.titleId}
                    </span>
                    <code className="shrink-0 font-mono text-xs text-[var(--color-muted)]">
                      {g.titleId}
                    </code>
                  </button>
                ))}
              </div>
              <p className="mt-1.5 text-xs text-[var(--color-muted)]">
                {tr(
                  "cheats_your_games_hint",
                  undefined,
                  "Click a game to find cheats for it, or search below.",
                )}
              </p>
            </div>
          )}

          {loadingGames && games.length === 0 && (
            <div className="mb-3 flex items-center gap-2 text-xs text-[var(--color-muted)]">
              <Spinner size={12} />
              {tr("cheats_loading_games", undefined, "Loading your games\u2026")}
            </div>
          )}

          <div className="flex gap-2">
            <input
              type="text"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void handleSearch()}
              placeholder={tr(
                "cheats_search_placeholder",
                undefined,
                "Search by game name or CUSA ID...",
              )}
              className="input flex-1"
              autoFocus
            />
            <Button
              onClick={handleSearch}
              disabled={searching}
              leftIcon={
                searching ? (
                  <Spinner size={14} tone="inherit" />
                ) : (
                  <Search size={14} />
                )
              }
            >
              {tr("cheats_search_action", undefined, "Search")}
            </Button>
          </div>

          {/* Only shown once there is something to narrow. Cheats are
              version-specific, so version is the filter that decides whether a
              download will work at all — not a nicety. */}
          {entries.length > 0 && (
            <div className="flex flex-wrap items-center gap-2 text-xs">
              <select
                className="input py-1 text-xs"
                value={filters.format}
                onChange={(ev) => setFilters((f) => ({ ...f, format: ev.target.value }))}
              >
                <option value="">{tr("cheats_filter_any_format", undefined, "Any format")}</option>
                {options.formats.map((f) => (
                  <option key={f} value={f}>{f.toUpperCase()}</option>
                ))}
              </select>
              <select
                className="input py-1 text-xs"
                value={filters.version}
                onChange={(ev) => setFilters((f) => ({ ...f, version: ev.target.value }))}
              >
                <option value="">{tr("cheats_filter_any_version", undefined, "Any game version")}</option>
                {options.versions.map((v) => (
                  <option key={v} value={v}>{v}</option>
                ))}
              </select>
              <label className="flex items-center gap-1.5">
                <input
                  type="checkbox"
                  checked={filters.installedOnly}
                  onChange={(ev) => setFilters((f) => ({ ...f, installedOnly: ev.target.checked }))}
                />
                {tr("cheats_filter_installed_only", undefined, "Only my games")}
              </label>
              <span className="text-[var(--color-muted)]">
                {tr("cheats_filter_count", { shown: visible.length, total: entries.length },
                  `${visible.length} of ${entries.length}`)}
              </span>
            </div>
          )}

          {error && <ErrorCard title={error} />}
          {downloadError && <ErrorCard title={downloadError} />}

          {visible.length > 0 && (
            <div className="space-y-1.5">
              {visible.map((e) => (
                <Card key={e.filename}>
                  <div className="flex items-center justify-between gap-3 p-3">
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">
                        {displayTitle(e, namesByTitleId)}
                      </div>
                      <div className="truncate font-mono text-xs text-[var(--color-muted)]">
                        {e.filename}
                      </div>
                      <span className="badge mt-1 inline-block text-xs uppercase">
                        {e.format}
                      </span>
                    </div>
                    {downloaded.has(e.filename) ? (
                      <span className="flex flex-shrink-0 items-center gap-1.5 text-sm font-medium text-[var(--color-good)]">
                        <CheckCircle2 size={16} />
                        {tr("cheats_installed", undefined, "Installed")}
                      </span>
                    ) : (
                      <Button
                        variant="primary"
                        size="sm"
                        className="flex-shrink-0"
                        onClick={() => void handleDownload(e)}
                        disabled={downloading === e.filename}
                        leftIcon={
                          downloading === e.filename ? (
                            <Spinner size={14} tone="inherit" />
                          ) : (
                            <Download size={14} />
                          )
                        }
                      >
                        {downloading === e.filename
                          ? tr("cheats_installing", undefined, "Installing\u2026")
                          : tr("cheats_install", undefined, "Install")}
                      </Button>
                    )}
                  </div>
                </Card>
              ))}
            </div>
          )}

          {!searching && entries.length > 0 && visible.length === 0 && (
            <div className="p-4 text-center text-sm text-[var(--color-muted)]">
              {tr("cheats_filter_none", undefined,
                "No cheats match these filters. Clear one to see more.")}
            </div>
          )}

          {!searching && entries.length === 0 && !error && (
            <div className="py-8 text-center text-sm text-[var(--color-muted)]">
              {tr(
                "cheats_search_hint",
                undefined,
                "Enter a game name or title ID (e.g. CUSA00001) and press Search",
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
