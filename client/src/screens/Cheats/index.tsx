import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Gamepad2,
  RefreshCw,
  Power,
  Trash2,
  ToggleLeft,
  ToggleRight,
  ChevronRight,
  Zap,
  Download,
} from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
  EmptyState,
  Card,
  Spinner,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import {
  cheatsList,
  cheatsGet,
  cheatsToggle,
  cheatsDelete,
  cheatsReload,
  cheatsStatus,
  cheatsEngineSet,
  appsInstalled,
  type InstalledTitle,
  type CheatTitle,
  type CheatMod,
  type CheatsStatusResponse,
  cheatsReposSearch,
} from "../../api/ps5";
import { RepoBrowser } from "./RepoBrowser";
import {
  isUsableGameTitle,
  namesFromRepoEntries,
  resolveCheatName,
} from "../../lib/cheatBrowse";

/** Repo names live for the session, not the component.
 *
 *  The index is a few hundred KB across three repos and changes about as often
 *  as somebody publishes a cheat, so re-fetching it every time the user opens
 *  the Cheats screen is pure latency. */
let repoNameCache = new Map<string, string>();

export default function CheatsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";

  /* Names for the games on this console.
   *
   * A cheat file only carries a game name in the .json format; .shn and
   * .mc4 carry none, so those rows showed a bare title id twice over.
   * The console knows what the game is called, so ask it. */
  const [installed, setInstalled] = useState<InstalledTitle[]>([]);
  useEffect(() => {
    void (async () => {
      if (!addr || payloadStatus !== "up") return;
      try {
        const r = await appsInstalled(addr);
        setInstalled(r.titles);
      } catch {
        // Non-fatal: rows fall back to the title id, as before.
        setInstalled([]);
      }
    })();
  }, [addr, payloadStatus]);


  const namesByTitleId = useMemo(() => {
    const m = new Map<string, string>();
    for (const g of installed) {
      if (g.titleId && g.titleName) m.set(g.titleId.toUpperCase(), g.titleName);
    }
    return m;
  }, [installed]);

  const [titles, setTitles] = useState<CheatTitle[]>([]);
  const [status, setStatus] = useState<CheatsStatusResponse | null>(null);
  const [loading, setLoading] = useState(false);
  /* Names from the cheat repos, for games that are NOT installed here.
   *
   * The installed list above only covers games on this console, so a cheat
   * downloaded for anything else showed a bare title id — the complaint in
   * issue #315. The repo index already maps every published cheat's title id
   * to its game name, so ask it once and keep the answer for the session;
   * it is one fetch of data the browser downloads anyway. */
  const [repoNames, setRepoNames] = useState<Map<string, string>>(repoNameCache);
  useEffect(() => {
    // Only worth a network round trip when something is ACTUALLY unnamed.
    // Most users have the game installed, in which case the console already
    // told us its name and fetching three repo indexes would be a stall that
    // bought nothing. Cached for the session so revisiting the screen is free.
    const unresolved = titles.some(
      (t) =>
        !isUsableGameTitle(t.name) &&
        !isUsableGameTitle(namesByTitleId.get(t.title_id.toUpperCase())) &&
        !repoNameCache.has(t.title_id.toUpperCase()),
    );
    if (!unresolved || repoNameCache.size > 0) return;
    void (async () => {
      try {
        const r = await cheatsReposSearch("");
        repoNameCache = namesFromRepoEntries(r.entries ?? []);
        setRepoNames(repoNameCache);
      } catch {
        // Offline or a repo is down. Names simply fall back as before —
        // never a reason to show an error on a screen about cheats.
      }
    })();
  }, [titles, namesByTitleId]);

  const [error, setError] = useState<string | null>(null);
  const [selectedTitle, setSelectedTitle] = useState<string | null>(null);
  const [mods, setMods] = useState<CheatMod[]>([]);
  const [modsLoading, setModsLoading] = useState(false);
  const [modsError, setModsError] = useState<string | null>(null);
  const [toggling, setToggling] = useState<number | null>(null);
  const [showRepoBrowser, setShowRepoBrowser] = useState(false);

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      const [list, st] = await Promise.all([
        cheatsList(addr),
        cheatsStatus(addr).catch(() => null),
      ]);
      setTitles(list.titles ?? []);
      setStatus(st);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, payloadStatus]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const loadMods = useCallback(
    async (titleId: string) => {
      if (!addr) return;
      setSelectedTitle(titleId);
      setMods([]);
      setModsError(null);
      setModsLoading(true);
      try {
        const resp = await cheatsGet(titleId, addr);
        setMods(resp.mods ?? []);
        if (resp.error) setModsError(resp.error);
      } catch (e) {
        setModsError(humanizePs5Error(String(e)));
      } finally {
        setModsLoading(false);
      }
    },
    [addr],
  );

  const handleToggle = async (index: number, currentOn: boolean) => {
    if (!addr || !selectedTitle) return;
    setToggling(index);
    try {
      const resp = await cheatsToggle(selectedTitle, index, !currentOn, addr);
      if (!resp.ok) {
        setModsError(resp.err || "Toggle failed");
      } else {
        setMods((prev) =>
          prev.map((m) =>
            m.index === index ? { ...m, on: !currentOn } : m,
          ),
        );
        setModsError(null);
      }
    } catch (e) {
      setModsError(humanizePs5Error(String(e)));
    } finally {
      setToggling(null);
    }
  };

  const handleDelete = async () => {
    if (!addr || !selectedTitle) return;
    if (!confirm(`Delete all cheat files for ${selectedTitle}?`)) return;
    try {
      await cheatsDelete(selectedTitle, addr);
      setSelectedTitle(null);
      setMods([]);
      void refresh();
    } catch (e) {
      setModsError(humanizePs5Error(String(e)));
    }
  };

  const handleReload = async () => {
    if (!addr) return;
    try {
      await cheatsReload(addr);
      void refresh();
      if (selectedTitle) void loadMods(selectedTitle);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    }
  };

  const handleEngineToggle = async () => {
    if (!addr || !status) return;
    try {
      const resp = await cheatsEngineSet(!status.enabled, addr);
      if (resp.ok) {
        setStatus((prev) =>
          prev ? { ...prev, enabled: resp.enabled } : prev,
        );
      }
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    }
  };

  return (
    <div className="mx-auto max-w-5xl space-y-4 p-4">
      <PageHeader
        icon={Gamepad2}
        title={tr("cheats_title", undefined, "Cheats")}
        description={tr(
          "cheats_description",
          undefined,
          "Apply memory patches to running games. Supports JSON, SHN, and patch files.",
        )}
        right={
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowRepoBrowser(true)}
              disabled={payloadStatus !== "up" || !addr}
              title={tr("cheats_download_title", undefined, "Download Community Cheats")}
            >
              <Download size={14} />
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={handleReload}
              disabled={loading || payloadStatus !== "up" || !addr}
            >
              {loading ? (
                <Spinner size={14} tone="inherit" />
              ) : (
                <RefreshCw size={14} />
              )}
            </Button>
          </div>
        }
      />

      <ConnectionGate>
        {error && <ErrorCard title={error} />}

        {status && (
          <Card>
            <div className="flex flex-wrap items-center justify-between gap-3 p-4">
              <div className="flex items-center gap-3">
                <Power
                  size={18}
                  className={
                    status.enabled
                      ? "text-[var(--color-good)]"
                      : "text-[var(--color-muted)]"
                  }
                />
                <div>
                  <div className="text-sm font-medium">
                    {tr(
                      "cheats_engine",
                      undefined,
                      "Cheat Engine",
                    )}
                  </div>
                  <div className="text-xs text-[var(--color-muted)]">
                    {status.enabled
                      ? tr("cheats_status_enabled", "Enabled")
                      : tr("cheats_status_disabled", "Disabled")}
                    {status.game_running && (
                      <span className="ml-2 text-[var(--color-accent)]">
                        {tr("cheats_game_label", "Game:")}{" "}
                        {status.game_title_id ||
                          tr("cheats_unknown", "unknown")}{" "}
                        {tr("cheats_pid_label", "(PID:")} {status.game_pid})
                      </span>
                    )}
                  </div>
                </div>
              </div>
              <div className="flex items-center gap-3">
                {status.patches_total > 0 && (
                  <span className="flex items-center gap-1 text-xs text-[var(--color-muted)]">
                    <Zap size={12} />
                    {status.patches_total}{" "}
                    {tr("cheats_patches_applied", "patches applied")}
                  </span>
                )}
                <Button
                  variant={status.enabled ? "primary" : "ghost"}
                  size="sm"
                  onClick={handleEngineToggle}
                >
                  {status.enabled ? tr("cheats_disable", "Disable") : tr("cheats_enable", "Enable")}
                </Button>
              </div>
            </div>
          </Card>
        )}

        <div className="grid gap-4 md:grid-cols-[280px_1fr]">
          {/* Title list */}
          <div className="space-y-2">
            <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
              {tr("cheats_titles", undefined, "Titles")}
            </h3>
            {titles.length === 0 && !loading ? (
              <EmptyState
                icon={Gamepad2}
                title={tr(
                  "cheats_no_titles",
                  undefined,
                  "No cheat files",
                )}
                message={tr(
                  "cheats_no_titles_hint",
                  undefined,
                  "Upload cheat files to /data/ps5upload/cheats/ on the PS5",
                )}
              />
            ) : (
              <div className="space-y-1">
                {titles.map((t) => (
                  <button
                    key={t.title_id}
                    onClick={() => void loadMods(t.title_id)}
                    className={`flex w-full items-center justify-between rounded-md border px-3 py-2 text-left text-sm transition-colors ${
                      selectedTitle === t.title_id
                        ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
                        : "border-[var(--color-border)] bg-[var(--color-surface-2)] hover:border-[var(--color-accent)]/40"
                    }`}
                  >
                    <div className="min-w-0">
                      <div className="truncate font-medium">
                        {resolveCheatName(t.title_id, {
                          fromCheatFile: t.name,
                          installed: namesByTitleId,
                          fromRepoIndex: repoNames,
                        })}
                      </div>
                      <div className="font-mono text-xs text-[var(--color-muted)]">
                        {t.title_id}
                        {t.version && (
                          <span className="ml-1.5 opacity-80">
                            {tr("cheats_version_label", undefined, "v")}
                            {t.version}
                          </span>
                        )}
                      </div>
                    </div>
                    <div className="flex items-center gap-2">
                      {t.running && (
                        <span className="flex items-center gap-1 text-xs text-[var(--color-good)]">
                          <span className="inline-block h-2 w-2 rounded-full bg-[var(--color-good)]" />
                        </span>
                      )}
                      <ChevronRight size={14} className="text-[var(--color-muted)]" />
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Mod list */}
          <div className="space-y-2">
            {!selectedTitle ? (
              <EmptyState
                icon={ToggleLeft}
                title={tr(
                  "cheats_select_title",
                  undefined,
                  "Select a title",
                )}
                message={tr(
                  "cheats_select_title_hint",
                  undefined,
                  "Choose a game from the list to view available cheats",
                )}
              />
            ) : (
              <>
                <div className="flex items-center justify-between">
                  <h3 className="text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
                    {tr("cheats_mods", undefined, "Mods")} — {selectedTitle}
                  </h3>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={handleDelete}
                  >
                    <Trash2 size={14} />
                  </Button>
                </div>
                {modsError && <ErrorCard title={modsError} />}
                {modsLoading ? (
                  <div className="flex items-center justify-center py-8">
                    <Spinner size={20} />
                  </div>
                ) : mods.length === 0 ? (
                  <EmptyState
                    icon={ToggleLeft}
                    title={tr(
                      "cheats_no_mods",
                      undefined,
                      "No mods found",
                    )}
                    message={tr(
                      "cheats_no_mods_hint",
                      undefined,
                      "This title has no cheat file or it is empty",
                    )}
                  />
                ) : (
                  <div className="space-y-2">
                    {mods.map((m) => (
                      <div
                        key={m.index}
                        className={`rounded-md border px-3 py-2.5 ${
                          m.on
                            ? "border-[var(--color-good)]/40 bg-[var(--color-good-soft)]"
                            : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
                        }`}
                      >
                        <div className="flex items-start justify-between gap-3">
                          <div className="min-w-0 flex-1">
                            <div className="text-sm font-medium">
                              {m.name || `Mod #${m.index}`}
                            </div>
                            {m.desc && (
                              <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                                {m.desc}
                              </div>
                            )}
                            <div className="mt-1 flex items-center gap-2 text-xs text-[var(--color-muted)]">
                              <span className="font-mono">{m.type}</span>
                            </div>
                          </div>
                          <button
                            onClick={() => void handleToggle(m.index, m.on)}
                            disabled={toggling === m.index}
                            className="flex-shrink-0"
                          >
                            {toggling === m.index ? (
                              <Spinner size={20} tone="inherit" />
                            ) : m.on ? (
                              <ToggleRight
                                size={24}
                                className="text-[var(--color-good)]"
                              />
                            ) : (
                              <ToggleLeft
                                size={24}
                                className="text-[var(--color-muted)]"
                              />
                            )}
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      </ConnectionGate>

      {showRepoBrowser && addr && (
        <RepoBrowser
          addr={addr}
          onDownloaded={() => void refresh()}
          onClose={() => setShowRepoBrowser(false)}
        />
      )}
    </div>
  );
}
