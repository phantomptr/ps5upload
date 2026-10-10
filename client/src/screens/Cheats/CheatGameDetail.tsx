import { useCallback, useEffect, useMemo, useState } from "react";
import { ArrowLeft, CheckCircle2, Download, Trash2 } from "lucide-react";

import {
  Button,
  Callout,
  ErrorCard,
  GameIcon,
  PlatformBadge,
  Spinner,
  Toggle,
} from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import {
  cheatsGet,
  cheatsDelete,
  cheatsReposDownload,
  cheatsToggle,
  type AppInfoDetails,
  type CheatMod,
  type CheatRepoEntry,
} from "../../api/ps5";
import { hostOf } from "../../lib/addr";
import { formatBytes } from "../../lib/format";
import { humanizePs5Error } from "../../lib/humanizeError";
import { hasVersionMatch, rankCheatFiles, sameVersion, type CheatGame } from "../../lib/cheatGames";
import { platformForTitleId } from "../../lib/titleDetails";
import { useTr } from "../../state/lang";
import { useToast } from "../../state/toasts";

/** Where a repo keeps its files, in words. */
function sourceName(repoId: string): string {
  if (repoId === "etahen") return "etaHEN";
  if (repoId === "henmix") return "HENmix";
  return repoId;
}

/** "2026-09-29 02:22:46.000" → the viewer's local date. appinfo.db stores
 *  UTC without a zone marker. */
function formatDbDate(v: string | null): string | null {
  if (!v) return null;
  const d = new Date(v.replace(" ", "T").replace(/\.\d+$/, "") + "Z");
  return Number.isNaN(d.getTime()) ? v : d.toLocaleDateString();
}

/** One game: who it is, the cheats on the console to switch on, and the
 *  cheats in the collection to download — in the order a player needs them. */
export function CheatGameDetail({
  host,
  addr,
  game,
  details,
  onChanged,
  onBack,
}: {
  host: string;
  addr: string;
  game: CheatGame;
  details: AppInfoDetails | null | undefined;
  onChanged: () => Promise<void> | void;
  onBack: () => void;
}) {
  const tr = useTr();
  const { toast } = useToast();
  const { confirm, dialog } = useConfirm();
  const [mods, setMods] = useState<CheatMod[]>([]);
  const [modsLoading, setModsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [toggling, setToggling] = useState<number | null>(null);
  const [downloading, setDownloading] = useState<string | null>(null);
  const [fetched, setFetched] = useState<Set<string>>(new Set());

  const version = details?.version ?? null;
  const files = useMemo(() => rankCheatFiles(game.available, version), [game.available, version]);
  const noMatch = !!version && files.length > 0 && !hasVersionMatch(files, version);
  const platform = platformForTitleId(game.titleId);

  const loadMods = useCallback(async () => {
    if (!game.downloaded) {
      setMods([]);
      return;
    }
    setModsLoading(true);
    try {
      const r = await cheatsGet(game.titleId, addr);
      setMods(r.mods ?? []);
      setError(r.error ?? null);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setModsLoading(false);
    }
  }, [addr, game.titleId, game.downloaded]);

  // The pane is keyed by title id, so `fetched` starts empty per game; it
  // must survive the reload a download triggers, or the file just fetched
  // would offer "Download" again.
  useEffect(() => {
    void loadMods();
  }, [loadMods]);

  async function download(e: CheatRepoEntry) {
    setDownloading(e.filename);
    setError(null);
    try {
      const r = await cheatsReposDownload(e.repo_id, e.filename, game.titleId, addr);
      if (!r.ok) {
        setError(r.error || tr("cheats_download_failed", undefined, "The download didn't finish."));
        return;
      }
      setFetched((prev) => new Set(prev).add(e.filename));
      toast({
        tone: "success",
        message: tr("cheats_toast_loaded", { name: game.name }, `Cheats loaded for ${game.name}`),
      });
      // The game now has cheats on the console: refresh so the list moves
      // it to "Cheats ready" and its switches appear above.
      await onChanged();
    } catch (err) {
      setError(humanizePs5Error(String(err)));
    } finally {
      setDownloading(null);
    }
  }

  async function toggle(m: CheatMod) {
    setToggling(m.index);
    try {
      const r = await cheatsToggle(game.titleId, m.index, !m.on, addr);
      if (!r.ok) {
        setError(r.err || tr("cheats_toggle_failed", undefined, "The PS5 didn't switch that cheat."));
        return;
      }
      setMods((prev) => prev.map((x) => (x.index === m.index ? { ...x, on: !m.on } : x)));
      setError(null);
      // Switching a cheat on also turns the engine on (the payload does it),
      // so let the engine bar catch up.
      if (!m.on) {
        toast({
          tone: "success",
          message: tr("cheats_toast_applied", { name: m.name || `#${m.index}` }, `${m.name || `#${m.index}`} applied`),
        });
        void onChanged();
      }
    } catch (err) {
      setError(humanizePs5Error(String(err)));
    } finally {
      setToggling(null);
    }
  }

  async function removeAll() {
    const ok = await confirm({
      title: tr("cheats_remove_title", undefined, "Remove this game's cheats?"),
      message: tr(
        "cheats_remove_body",
        { name: game.name },
        `This deletes every cheat file for ${game.name} from the PS5. You can download them again.`,
      ),
      confirmLabel: tr("cheats_remove", undefined, "Remove"),
      destructive: true,
    });
    if (!ok) return;
    try {
      await cheatsDelete(game.titleId, addr);
      setMods([]);
      await onChanged();
    } catch (err) {
      setError(humanizePs5Error(String(err)));
    }
  }

  const onCount = mods.filter((m) => m.on).length;
  const facts: [string, string | null][] = [
    [tr("cheats_fact_version", undefined, "Version"), version],
    [tr("cheats_fact_size", undefined, "Size"), details?.sizeBytes ? formatBytes(details.sizeBytes) : null],
    [tr("cheats_fact_installed", undefined, "Installed"), formatDbDate(details?.installedAt ?? null)],
    [tr("cheats_fact_last_opened", undefined, "Last opened"), formatDbDate(details?.lastOpenedAt ?? null)],
    [tr("cheats_fact_content_id", undefined, "Content ID"), details?.contentId ?? null],
  ];

  return (
    <div className="flex min-w-0 flex-col gap-4">
      <button
        type="button"
        onClick={onBack}
        className="flex items-center gap-1 self-start text-sm text-[var(--color-muted)] hover:text-[var(--color-text)] md:hidden"
      >
        <ArrowLeft size={14} />
        {tr("cheats_back", undefined, "All games")}
      </button>

      {/* Who the game is. */}
      <div className="flex flex-col gap-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 sm:flex-row">
        <GameIcon host={hostOf(host)} titleId={game.titleId} size={112} rounded="rounded-lg" />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="text-lg font-semibold [overflow-wrap:anywhere]">{game.name}</h2>
            {platform && <PlatformBadge platform={platform} />}
            {game.running && (
              <span className="inline-flex items-center gap-1 rounded-full bg-[var(--color-good)]/15 px-2 py-0.5 text-xs font-medium text-[var(--color-good)]">
                <span className="h-1.5 w-1.5 rounded-full bg-[var(--color-good)]" />
                {tr("cheats_badge_playing", undefined, "Playing")}
              </span>
            )}
          </div>
          <div className="mt-0.5 font-mono text-xs text-[var(--color-muted)]">{game.titleId}</div>
          {game.installed ? (
            <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-2 text-xs sm:grid-cols-3">
              {facts
                .filter(([, v]) => v)
                .map(([k, v]) => (
                  <div key={k} className="min-w-0">
                    <dt className="text-[var(--color-muted)]">{k}</dt>
                    <dd className="truncate font-medium tabular-nums" title={v ?? undefined}>
                      {v}
                    </dd>
                  </div>
                ))}
            </dl>
          ) : (
            <p className="mt-3 text-xs text-[var(--color-muted)]">
              {tr("cheats_not_installed", undefined, "Not installed on this PS5.")}
            </p>
          )}
        </div>
      </div>

      {error && <ErrorCard title={error} />}

      {/* Step: switch cheats on. Only once some are on the console. */}
      {game.downloaded && (
        <section className="surface-panel p-5">
          <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
            <div>
              <h3 className="text-sm font-semibold">
                {tr("cheats_on_ps5", undefined, "Cheats on your PS5")}
              </h3>
              <p className="text-xs text-[var(--color-muted)]">
                {mods.length > 0
                  ? tr(
                      "cheats_on_count",
                      { on: onCount, total: mods.length },
                      `${onCount} of ${mods.length} on`,
                    )
                  : tr("cheats_on_hint", undefined, "Switch on the ones you want.")}
                {game.downloadedVersion &&
                  ` · ${tr("cheats_for_version", { v: game.downloadedVersion }, `for v${game.downloadedVersion}`)}`}
              </p>
            </div>
            <Button variant="ghost" size="sm" leftIcon={<Trash2 size={13} />} onClick={() => void removeAll()}>
              {tr("cheats_remove", undefined, "Remove")}
            </Button>
          </div>

          {/* The console refuses a switch while the game isn't running ("no
              game is currently running"), so say so before anyone tries. */}
          {!game.running && mods.length > 0 && (
            <Callout
              tone="info"
              className="mb-3"
              title={tr("cheats_start_game_title", undefined, "Start the game to switch cheats on")}
            >
              {tr(
                "cheats_start_game_body",
                { name: game.name },
                `The PS5 applies cheats to a running game, so the switches work once ${game.name} is open.`,
              )}
            </Callout>
          )}
          {game.downloadedVersion && version && !sameVersion(game.downloadedVersion, version) && (
            <Callout tone="warn" className="mb-3" title={tr("cheats_version_mismatch_title", undefined, "Made for a different version")}>
              {tr(
                "cheats_version_mismatch_body",
                { cheat: game.downloadedVersion, game: version },
                `These cheats are for v${game.downloadedVersion}, and your game is v${version}. They may do nothing, or crash the game.`,
              )}
            </Callout>
          )}

          {modsLoading ? (
            <div className="flex justify-center py-6">
              <Spinner size={20} />
            </div>
          ) : mods.length === 0 ? (
            <p className="text-sm text-[var(--color-muted)]">
              {tr("cheats_no_mods_hint", undefined, "This title has no cheat file or it is empty")}
            </p>
          ) : (
            <ul className="grid gap-1">
              {mods.map((m) => (
                <li
                  key={m.index}
                  className={`flex items-center gap-2 rounded-md px-2 py-2 ${
                    m.on ? "bg-[var(--color-accent-soft)]" : "hover:bg-[var(--color-surface-2)]"
                  }`}
                >
                  <Toggle
                    className="min-w-0 flex-1"
                    checked={m.on}
                    disabled={toggling === m.index || !game.running}
                    onChange={() => void toggle(m)}
                    label={m.name || `Cheat #${m.index}`}
                    hint={m.desc || undefined}
                  />
                  {toggling === m.index && <Spinner size={14} />}
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      {/* Step: get cheats. */}
      <section className="surface-panel p-5">
        <h3 className="text-sm font-semibold">
          {game.downloaded
            ? tr("cheats_more_title", undefined, "More cheats for this game")
            : tr("cheats_get_title", undefined, "Download cheats")}
        </h3>
        <p className="mb-3 text-xs text-[var(--color-muted)]">
          {tr(
            "cheats_get_hint",
            undefined,
            "Cheats are made for one game version. Pick the one that matches yours.",
          )}
        </p>

        {noMatch && (
          <Callout tone="warn" className="mb-3" title={tr("cheats_no_match_title", { v: version ?? "" }, `Nothing for v${version} yet`)}>
            {tr(
              "cheats_no_match_body",
              undefined,
              "These are for other versions of the game. They usually do nothing, and can crash it.",
            )}
          </Callout>
        )}

        {files.length === 0 ? (
          <p className="text-sm text-[var(--color-muted)]">
            {tr("cheats_none_published", undefined, "Nobody has published cheats for this game yet.")}
          </p>
        ) : (
          <ul className="grid gap-1.5">
            {files.map((e) => {
              const match = sameVersion(e.game_version, version);
              const done = fetched.has(e.filename);
              return (
                <li
                  key={`${e.repo_id}/${e.filename}`}
                  className={`flex flex-wrap items-center gap-3 rounded-md border px-3 py-2 ${
                    match ? "border-[var(--color-good)]/50 bg-[var(--color-good)]/5" : "border-[var(--color-border)]"
                  }`}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2 text-sm">
                      <span className="font-medium tabular-nums">
                        {e.game_version ? `v${e.game_version}` : tr("cheats_any_version", undefined, "Any version")}
                      </span>
                      {match && (
                        <span className="rounded-full bg-[var(--color-good)]/15 px-2 py-0.5 text-[11px] font-medium text-[var(--color-good)]">
                          {tr("cheats_your_version", undefined, "Your version")}
                        </span>
                      )}
                    </div>
                    <div className="mt-0.5 truncate text-xs text-[var(--color-muted)]" title={e.filename}>
                      {e.format.toUpperCase()} · {sourceName(e.repo_id)} · {e.filename}
                    </div>
                  </div>
                  {done ? (
                    <span className="flex items-center gap-1.5 text-sm font-medium text-[var(--color-good)]">
                      <CheckCircle2 size={15} />
                      {tr("cheats_downloaded", undefined, "Downloaded")}
                    </span>
                  ) : (
                    <Button
                      variant={match || !version ? "primary" : "secondary"}
                      size="sm"
                      onClick={() => void download(e)}
                      disabled={downloading !== null}
                      leftIcon={downloading === e.filename ? <Spinner size={13} tone="inherit" /> : <Download size={13} />}
                    >
                      {downloading === e.filename
                        ? tr("cheats_downloading", undefined, "Downloading…")
                        : tr("cheats_download", undefined, "Download")}
                    </Button>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </section>
      {dialog}
    </div>
  );
}
