import { useCallback, useEffect, useRef, useState } from "react";
import { pickPaths } from "../../lib/pickPath";
import {
  ArrowDown,
  ArrowUp,
  CheckCircle2,
  CircleDashed,
  Clock,
  FilePlus,
  FolderOpen,
  Loader2,
  ListPlus,
  Pencil,
  Play,
  Plus,
  Square,
  Trash2,
  X,
  XCircle,
  Zap,
} from "lucide-react";
import { getCurrentWebview } from "@tauri-apps/api/webview";

import { Button, Modal } from "../../components";
import { ConsoleChip } from "../../components/ConsoleChip";
import { useTr } from "../../state/lang";
import {
  runStatusForHost,
  usePayloadPlaylistsStore,
} from "../../state/payloadPlaylists";
import {
  isPayloadPath,
  sanitiseSleepMs,
  type Playlist,
} from "../../lib/playlistOps";
import { isAndroid } from "../../lib/platform";
import { isTauriEnv, safeUnlisten } from "../../lib/tauriEnv";
import { log } from "../../state/logs";

/**
 * Playlist editor + runner panel for the SendPayload screen.
 *
 * Each playlist is an ordered list of (payload_path, sleep_ms_after)
 * steps. Users create playlists, add steps via the same file picker
 * the main form uses, and click Run to send each payload in sequence
 * with the configured sleep between steps. The store handles
 * persistence + cancellation; this component is purely the UI.
 */
export function PlaylistsPanel({ host, port }: { host: string; port: number }) {
  const tr = useTr();
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  const loaded = usePayloadPlaylistsStore((s) => s.loaded);
  const runStatus = usePayloadPlaylistsStore((s) => runStatusForHost(s, host));
  const hydrate = usePayloadPlaylistsStore((s) => s.hydrate);
  const createPlaylist = usePayloadPlaylistsStore((s) => s.createPlaylist);
  const stop = usePayloadPlaylistsStore((s) => s.stop);
  const run = usePayloadPlaylistsStore((s) => s.run);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  // Create a playlist from a set of paths (dropped or multi-selected),
  // keeping only real payload files. Reads the store via getState() rather
  // than closing over the reactive `playlists` value, so the drag-drop
  // effect below can depend on a STABLE callback (no re-subscribe churn).
  // Returns the number of steps added (0 = nothing usable was supplied).
  const createFromPaths = useCallback((paths: string[]): number => {
    const payloads = paths.filter(isPayloadPath);
    if (payloads.length === 0) {
      // Leave a trace when a drop/pick produced nothing usable, so a user who
      // dropped (say) a .pkg and saw "nothing happen" shows up in a bug report.
      if (paths.length > 0) {
        log.warn(
          "playlist",
          `drop/pick ignored — no payload files among ${paths.length} path(s)`,
        );
      }
      return 0;
    }
    const st = usePayloadPlaylistsStore.getState();
    const base =
      basename(payloads[0]).replace(/\.[^.]+$/, "") || "New playlist";
    const existing = new Set(st.playlists.map((p) => p.name));
    let name = base;
    let n = 2;
    while (existing.has(name)) name = `${base} ${n++}`;
    const id = st.createPlaylist(name);
    for (const p of payloads) st.addStep(id, { path: p, sleepMs: 0 });
    log.info(
      "playlist",
      `created "${name}" with ${payloads.length} step(s) from drop/pick`,
    );
    return payloads.length;
  }, []);

  // Drag one or more payload files onto the DROPZONE to spin up a playlist in
  // one gesture. Desktop only — Android has no webview drag-drop, so the
  // "From files…" multi-picker below covers it there.
  const [dropActive, setDropActive] = useState(false);
  // Tauri's drag-drop is a single window-global event with no element
  // targeting, so we hit-test the drop position against the dropzone's rect.
  // Without this, dropping a payload file ANYWHERE on the Payloads screen
  // (sidebar, a card, the send form) would silently fabricate a playlist.
  const dropZoneRef = useRef<HTMLDivElement | null>(null);
  const dragHasPayloadRef = useRef(false);
  useEffect(() => {
    if (!isTauriEnv() || isAndroid()) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    // Tauri reports drop position in PHYSICAL pixels; getBoundingClientRect is
    // CSS pixels — divide by devicePixelRatio to compare in the same space.
    const inZone = (pos: { x: number; y: number }): boolean => {
      const zone = dropZoneRef.current;
      if (!zone) return false;
      const r = zone.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      const x = pos.x / dpr;
      const y = pos.y / dpr;
      return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
    };
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (cancelled) return;
      if (e.payload.type === "enter") {
        dragHasPayloadRef.current = e.payload.paths.some(isPayloadPath);
        setDropActive(
          dragHasPayloadRef.current && inZone(e.payload.position),
        );
      } else if (e.payload.type === "over") {
        // "over" has no paths; reuse the flag captured on enter. Highlight only
        // while actually hovering the zone with a payload-bearing drag.
        setDropActive(
          dragHasPayloadRef.current && inZone(e.payload.position),
        );
      } else if (e.payload.type === "leave") {
        dragHasPayloadRef.current = false;
        setDropActive(false);
      } else if (e.payload.type === "drop") {
        setDropActive(false);
        dragHasPayloadRef.current = false;
        // Only OUR zone's drops build a playlist — a drop elsewhere on the
        // window is someone else's concern (or nobody's).
        if (inZone(e.payload.position)) createFromPaths(e.payload.paths);
      }
    });
    p.then((fn) => {
      if (cancelled) safeUnlisten(fn);
      else unlisten = fn;
    }).catch(() => {});
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, [createFromPaths]);

  const handleFromFiles = async () => {
    const picked = await pickPaths({
      filters: [
        { name: "Payload", extensions: ["elf", "bin", "js", "lua", "jar"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    createFromPaths(picked);
  };

  const handleNew = () => {
    // window.prompt doesn't work in Tauri webview by default — clicking
    // it returns null silently. Skip the modal entirely: create with
    // an auto-numbered default name and let users rename inline by
    // clicking the title in the card header. Quicker UX too — common
    // case is "make a playlist, drop steps in", renaming is rare.
    const baseName = tr("playlist_default_name", undefined, "New playlist");
    const existing = new Set(playlists.map((p) => p.name));
    let n = playlists.length + 1;
    let candidate = `${baseName} ${n}`;
    while (existing.has(candidate)) {
      n += 1;
      candidate = `${baseName} ${n}`;
    }
    createPlaylist(candidate);
  };

  const isBusy = runStatus.kind === "running" || runStatus.kind === "sleeping";

  // Most-recently-run playlists for the quick-access strip. lastRunAt is
  // stamped on every run() start, so this surfaces "the ones you actually
  // use" without the user scrolling the full list. Cap at 5 so the strip
  // stays one tidy row.
  const recentlyRun = playlists
    .filter((p) => typeof p.lastRunAt === "number")
    .sort((a, b) => (b.lastRunAt ?? 0) - (a.lastRunAt ?? 0))
    .slice(0, 5);

  return (
    <section className="mt-8 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5">
      <header className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <ListPlus size={14} />
            {tr("playlists_title", undefined, "Playlists")}
          </h2>
          <p className="mt-0.5 text-xs text-[var(--color-muted)]">
            {tr(
              "playlists_description",
              undefined,
              "Send a sequence of payloads with sleeps between each — useful for scripted boot orders (loader → patches → launcher).",
            )}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {isBusy && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Square size={12} />}
              onClick={() => stop(host)}
            >
              {tr("playlist_stop", undefined, "Stop run")}
            </Button>
          )}
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<FilePlus size={12} />}
            onClick={handleFromFiles}
            disabled={isBusy}
            title={
              isAndroid()
                ? tr(
                    "playlist_from_file_title_android",
                    undefined,
                    "Pick a payload to start a playlist (add more with Add step)",
                  )
                : tr(
                    "playlist_from_files_title",
                    undefined,
                    "Pick one or more payloads to make a playlist",
                  )
            }
          >
            {/* Android's picker is single-select, so 'From files…' (which
                implies bulk) would over-promise — label it honestly there. */}
            {isAndroid()
              ? tr("playlist_from_file_android", undefined, "From a file…")
              : tr("playlist_from_files", undefined, "From files…")}
          </Button>
          <Button
            variant="primary"
            size="sm"
            leftIcon={<Plus size={12} />}
            onClick={handleNew}
            disabled={isBusy}
          >
            {tr("playlist_new", undefined, "New playlist")}
          </Button>
        </div>
      </header>

      {/* Drag-to-create zone (desktop). Drop one or more payloads to spin up
          a playlist; auto-detected by extension, so a stray .pkg or folder
          in the selection is simply ignored. */}
      {!isAndroid() && (
        <div
          ref={dropZoneRef}
          className={`mb-4 rounded-md border-2 border-dashed p-3 text-center text-xs transition-colors ${
            dropActive
              ? "border-[var(--color-accent)] bg-[var(--color-surface-3)]"
              : "border-[var(--color-border)] text-[var(--color-muted)]"
          }`}
        >
          {tr(
            "playlist_drop_hint",
            undefined,
            "Drag one or more payloads here to create a playlist",
          )}
        </div>
      )}

      <RunStatusBanner host={host} />

      <AutoLoaderCard />

      {recentlyRun.length > 0 && (
        <div className="mb-4">
          <div className="mb-1.5 flex items-center gap-1.5 text-xs font-medium text-[var(--color-muted)]">
            <Clock size={12} />
            {tr("playlists_recently_run", undefined, "Recently run")}
          </div>
          <div className="flex flex-wrap gap-2">
            {recentlyRun.map((p) => {
              const canQuickRun =
                p.steps.length > 0 && !!host?.trim() && !isBusy;
              return (
                <div
                  key={p.id}
                  className="flex items-center gap-1.5 rounded-full border border-[var(--color-border)] bg-[var(--color-surface)] py-1 pl-3 pr-1 text-xs"
                >
                  <span className="max-w-[12rem] truncate font-medium">
                    {p.name}
                  </span>
                  <span className="text-xs text-[var(--color-muted)] tabular-nums">
                    {formatAgo(p.lastRunAt ?? 0)}
                  </span>
                  <button
                    type="button"
                    onClick={() => void run(p.id, host.trim(), port)}
                    disabled={!canQuickRun}
                    title={
                      !host?.trim()
                        ? tr(
                            "playlist_need_host",
                            undefined,
                            "Set a PS5 IP at the top of the screen first",
                          )
                        : p.steps.length === 0
                          ? tr(
                              "playlist_need_steps",
                              undefined,
                              "Add at least one step",
                            )
                          : tr(
                              "playlist_run_tooltip",
                              { name: p.name },
                              'Run "{name}"',
                            )
                    }
                    className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[var(--color-accent)] hover:bg-[var(--color-surface-3)] disabled:opacity-40"
                  >
                    <Play size={12} />
                  </button>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {playlists.length === 0 && (
        <div className="rounded-md border border-dashed border-[var(--color-border)] bg-[var(--color-surface)] p-4 text-center text-xs text-[var(--color-muted)]">
          {tr(
            "playlists_empty",
            undefined,
            "No playlists yet. Create one to script a boot sequence.",
          )}
        </div>
      )}

      <div className="grid gap-3">
        {playlists.map((p, i) => (
          <PlaylistCard
            key={p.id}
            playlist={p}
            host={host}
            port={port}
            anyRunning={isBusy}
            index={i}
            total={playlists.length}
          />
        ))}
      </div>
    </section>
  );
}

/**
 * Auto-loader config card. One playlist that runs by itself whenever any
 * console's helper payload becomes ready — covers "after first-time setup"
 * and "on reconnect" with a single edge (see AppShell's status poller). The
 * trigger lives in AppShell; this card is just the enable toggle + picker.
 */
function AutoLoaderCard() {
  const tr = useTr();
  const autoLoader = usePayloadPlaylistsStore((s) => s.autoLoader);
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  const setAutoLoader = usePayloadPlaylistsStore((s) => s.setAutoLoader);

  const selected = autoLoader.playlistId
    ? playlists.find((p) => p.id === autoLoader.playlistId)
    : undefined;
  // "Armed" = enabled AND it will actually do something. The icon lights up
  // only when armed so the state is legible at a glance.
  const armed = autoLoader.enabled && !!selected && selected.steps.length > 0;
  // Enabled but inert — surface why so the toggle doesn't look broken.
  const warn = autoLoader.enabled && (!selected || selected.steps.length === 0);

  return (
    <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-center sm:justify-between">
        <div className="flex min-w-0 items-start gap-2">
          <Zap
            size={14}
            className={`mt-0.5 shrink-0 ${
              armed ? "text-[var(--color-accent)]" : "text-[var(--color-muted)]"
            }`}
          />
          <div className="min-w-0">
            <div className="text-xs font-semibold">
              {tr("autoloader_title", undefined, "Auto-loader")}
            </div>
            <p className="mt-0.5 text-xs text-[var(--color-muted)]">
              {tr(
                "autoloader_description",
                undefined,
                "Automatically run a playlist whenever a PS5's helper becomes ready — after first-time setup or a reconnect.",
              )}
            </p>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <select
            value={autoLoader.playlistId ?? ""}
            onChange={(e) =>
              setAutoLoader({ playlistId: e.target.value || null })
            }
            className="min-w-[min(100%,10rem)] rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
          >
            <option value="">
              {tr("autoloader_pick", undefined, "Choose playlist…")}
            </option>
            {playlists.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
          <label className="flex shrink-0 items-center gap-1.5 text-xs font-medium">
            <input
              type="checkbox"
              checked={autoLoader.enabled}
              onChange={(e) => setAutoLoader({ enabled: e.target.checked })}
              className="h-3.5 w-3.5"
            />
            {tr("autoloader_enable", undefined, "Enable")}
          </label>
        </div>
      </div>
      {/* Bring-up playlist: the PRE-helper chain (kernel-R/W payload, SMP, …)
          that "Quick bring-up" (Connection screen) runs before sending the
          helper. Separate from the auto-loader's post-helper playlist above. */}
      <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
        <span className="text-[var(--color-muted)]">
          {tr(
            "autoloader_bringup_label",
            undefined,
            "Bring-up playlist (kernel R/W, SMP):",
          )}
        </span>
        <select
          value={autoLoader.bringUpPlaylistId ?? ""}
          onChange={(e) =>
            setAutoLoader({ bringUpPlaylistId: e.target.value || null })
          }
          className="min-w-[min(100%,10rem)] rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
        >
          <option value="">
            {tr("autoloader_bringup_none", undefined, "None (send helper only)")}
          </option>
          {playlists.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </div>
      {warn && (
        <div className="mt-2 text-xs text-[var(--color-warn)]">
          {!selected
            ? tr(
                "autoloader_warn_none",
                undefined,
                "Pick a playlist for the auto-loader to run.",
              )
            : tr(
                "autoloader_warn_empty",
                undefined,
                "The chosen playlist has no steps yet.",
              )}
        </div>
      )}
    </div>
  );
}

function RunStatusBanner({ host }: { host: string }) {
  const tr = useTr();
  const runStatus = usePayloadPlaylistsStore((s) => runStatusForHost(s, host));
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  // Pull `now` out of state so the 250 ms tick re-renders the banner
  // without calling `Date.now()` during render (lint forbids — render
  // must be a pure function of props/state). The tick effect updates
  // state with the current wall-clock; everything below is then a
  // pure function of that snapshot.
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    if (runStatus.kind !== "sleeping") return;
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(id);
  }, [runStatus.kind]);

  if (runStatus.kind === "idle") return null;
  const playlist = playlists.find(
    (p) => "playlistId" in runStatus && p.id === runStatus.playlistId,
  );
  const name = playlist?.name ?? "playlist";

  if (runStatus.kind === "running") {
    const step = playlist?.steps[runStatus.stepIndex];
    return (
      <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] p-2 text-xs">
        <Loader2
          size={14}
          className="animate-spin text-[var(--color-accent)]"
        />
        <span className="font-medium">{name}</span>
        <ConsoleChip addr={runStatus.host} />
        <span className="text-[var(--color-muted)]">
          {tr(
            "playlist_status_running",
            {
              index: runStatus.stepIndex + 1,
              total: playlist?.steps.length ?? 0,
            },
            "Sending step {index} of {total}",
          )}
          {step ? `: ${basename(step.path)}` : ""}
        </span>
      </div>
    );
  }

  if (runStatus.kind === "sleeping") {
    const elapsed = now - runStatus.sleepStartedAtMs;
    const remaining = Math.max(0, runStatus.sleepDurationMs - elapsed);
    return (
      <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] p-2 text-xs">
        <Clock size={14} className="text-[var(--color-accent)]" />
        <span className="font-medium">{name}</span>
        <ConsoleChip addr={runStatus.host} />
        <span className="text-[var(--color-muted)]">
          {tr(
            "playlist_status_sleeping",
            { remaining: Math.ceil(remaining / 1000) },
            "Sleeping {remaining}s before next step…",
          )}
        </span>
      </div>
    );
  }

  if (runStatus.kind === "done") {
    const hasFailures = runStatus.failureCount > 0;
    return (
      <div
        className={`mb-3 rounded-md border p-2 text-xs ${
          hasFailures
            ? "border-[var(--color-warn)] bg-[var(--color-surface)]"
            : "border-[var(--color-good)] bg-[var(--color-surface)]"
        }`}
      >
        <div className="flex items-center gap-2">
          {hasFailures ? (
            <XCircle size={14} className="text-[var(--color-warn)]" />
          ) : (
            <CheckCircle2 size={14} className="text-[var(--color-good)]" />
          )}
          <span className="font-medium">{name}</span>
          <span className="text-[var(--color-muted)]">
            {tr(
              "playlist_status_done",
              { ok: runStatus.successCount, fail: runStatus.failureCount },
              "Done — {ok} sent, {fail} failed",
            )}
          </span>
        </div>
        {/* Per-step failures: surfaced when continueOnFailure=true and
            anything broke. Without this, users only see "N failed"
            and have to dig through logs to figure out which step. */}
        {hasFailures && runStatus.failures.length > 0 && (
          <ul className="mt-1.5 space-y-0.5 pl-6 font-mono text-xs text-[var(--color-warn)]">
            {runStatus.failures.map((f) => (
              <li key={f.stepIndex} className="break-all">
                {tr(
                  "playlist_failed_step_line",
                  { step: f.stepIndex + 1, error: f.error },
                  "Step {step}: {error}",
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    );
  }

  return (
    <div className="mb-3 flex items-start gap-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
      <XCircle size={14} className="mt-0.5 shrink-0" />
      <div className="min-w-0 flex-1">
        <div className="font-medium">{name}</div>
        <div className="mt-0.5">
          {tr(
            "playlist_status_failed",
            { step: runStatus.stepIndex + 1, error: runStatus.error },
            "Step {step} failed: {error}",
          )}
        </div>
      </div>
    </div>
  );
}

function PlaylistCard({
  playlist,
  host,
  port,
  anyRunning,
  index,
  total,
}: {
  playlist: Playlist;
  host: string;
  port: number;
  anyRunning: boolean;
  index: number;
  total: number;
}) {
  const tr = useTr();
  const renamePlaylist = usePayloadPlaylistsStore((s) => s.renamePlaylist);
  const deletePlaylist = usePayloadPlaylistsStore((s) => s.deletePlaylist);
  const movePlaylistUp = usePayloadPlaylistsStore((s) => s.movePlaylistUp);
  const movePlaylistDown = usePayloadPlaylistsStore((s) => s.movePlaylistDown);
  const setContinueOnFailure = usePayloadPlaylistsStore(
    (s) => s.setContinueOnFailure,
  );
  const addStep = usePayloadPlaylistsStore((s) => s.addStep);
  const removeStep = usePayloadPlaylistsStore((s) => s.removeStep);
  const moveStepUp = usePayloadPlaylistsStore((s) => s.moveStepUp);
  const moveStepDown = usePayloadPlaylistsStore((s) => s.moveStepDown);
  const updateStep = usePayloadPlaylistsStore((s) => s.updateStep);
  const run = usePayloadPlaylistsStore((s) => s.run);
  const runStatus = usePayloadPlaylistsStore((s) => runStatusForHost(s, host));

  const isThisRunning =
    (runStatus.kind === "running" || runStatus.kind === "sleeping") &&
    "playlistId" in runStatus &&
    runStatus.playlistId === playlist.id;
  const activeStepIndex =
    isThisRunning && "stepIndex" in runStatus ? runStatus.stepIndex : -1;

  // Inline rename: click the name to swap into an editable input,
  // commit on blur or Enter, cancel on Escape. Avoids window.prompt
  // (broken in Tauri webview) and keeps the rename gesture in-place.
  const [renaming, setRenaming] = useState(false);
  const [draftName, setDraftName] = useState(playlist.name);
  const renameRef = useRef<HTMLInputElement | null>(null);
  // Custom delete-confirm modal — Tauri webview also no-ops
  // window.confirm, so we render an in-tree confirmation dialog.
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  useEffect(() => {
    if (renaming) {
      setDraftName(playlist.name);
      // Defer focus until after the input renders.
      requestAnimationFrame(() => {
        renameRef.current?.focus();
        renameRef.current?.select();
      });
    }
  }, [renaming, playlist.name]);

  const commitRename = () => {
    const trimmed = draftName.trim();
    if (trimmed.length > 0 && trimmed !== playlist.name) {
      renamePlaylist(playlist.id, trimmed);
    }
    setRenaming(false);
  };

  const handleAddStep = async () => {
    // Multi-select: pick several payloads at once and append them all as
    // steps (each with no post-send sleep — the user tunes timing after).
    const picked = await pickPaths({
      filters: [
        { name: "Payload", extensions: ["elf", "bin", "js", "lua", "jar"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    for (const path of picked) addStep(playlist.id, { path, sleepMs: 0 });
  };

  const canRun = playlist.steps.length > 0 && !!host?.trim() && !anyRunning;

  return (
    <article className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-4">
      <header className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          {/* Reorder this playlist within the list. Mirrors the per-step
              up/down controls; persisted order survives a restart. */}
          <div className="flex shrink-0 items-center">
            <button
              type="button"
              onClick={() => movePlaylistUp(playlist.id)}
              disabled={anyRunning || index === 0}
              className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
              title={tr("playlist_move_up", undefined, "Move playlist up")}
            >
              <ArrowUp size={12} />
            </button>
            <button
              type="button"
              onClick={() => movePlaylistDown(playlist.id)}
              disabled={anyRunning || index === total - 1}
              className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
              title={tr("playlist_move_down", undefined, "Move playlist down")}
            >
              <ArrowDown size={12} />
            </button>
          </div>
          {renaming ? (
            <input
              ref={renameRef}
              value={draftName}
              onChange={(e) => setDraftName(e.target.value)}
              onBlur={commitRename}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commitRename();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  setRenaming(false);
                }
              }}
              className="rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] px-2 py-0.5 text-sm font-semibold outline-none"
            />
          ) : (
            <button
              type="button"
              onClick={() => setRenaming(true)}
              disabled={anyRunning}
              className="flex items-center gap-1 text-sm font-semibold hover:text-[var(--color-accent)] disabled:opacity-60"
              title={tr(
                "playlist_rename_tooltip",
                undefined,
                "Click to rename",
              )}
            >
              <span className="truncate">{playlist.name}</span>
              <Pencil
                size={11}
                className="opacity-0 transition-opacity group-hover:opacity-60"
              />
            </button>
          )}
          <span className="text-xs text-[var(--color-muted)]">
            ·{" "}
            {tr(
              "playlist_step_count",
              { count: playlist.steps.length },
              `${playlist.steps.length} step${playlist.steps.length === 1 ? "" : "s"}`,
            )}
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-1.5 text-xs text-[var(--color-muted)]">
            <input
              type="checkbox"
              checked={playlist.continueOnFailure}
              onChange={(e) =>
                setContinueOnFailure(playlist.id, e.target.checked)
              }
              disabled={anyRunning}
              className="h-3 w-3"
            />
            {tr(
              "playlist_continue_on_failure",
              undefined,
              "Continue on failure",
            )}
          </label>
          <Button
            variant="primary"
            size="sm"
            leftIcon={<Play size={12} />}
            onClick={() => void run(playlist.id, host.trim(), port)}
            disabled={!canRun}
            title={
              !host?.trim()
                ? tr(
                    "playlist_need_host",
                    undefined,
                    "Set a PS5 IP at the top of the screen first",
                  )
                : playlist.steps.length === 0
                  ? tr(
                      "playlist_need_steps",
                      undefined,
                      "Add at least one step",
                    )
                  : tr(
                      "playlist_run_tooltip",
                      { name: playlist.name },
                      'Run "{name}"',
                    )
            }
          >
            {tr("playlist_run", undefined, "Run")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            leftIcon={<Trash2 size={12} />}
            onClick={() => setConfirmingDelete(true)}
            disabled={anyRunning}
            title={tr(
              "playlist_delete_tooltip",
              undefined,
              "Delete this playlist",
            )}
          >
            {tr("delete", undefined, "Delete")}
          </Button>
        </div>
      </header>

      {confirmingDelete && (
        <Modal
          open
          onClose={() => setConfirmingDelete(false)}
          role="alertdialog"
          size="md"
          title={tr(
            "playlist_delete_title",
            { name: playlist.name },
            'Delete "{name}"?',
          )}
          footer={
            <>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setConfirmingDelete(false)}
              >
                {tr("cancel", undefined, "Cancel")}
              </Button>
              <Button
                variant="danger"
                size="sm"
                onClick={() => {
                  setConfirmingDelete(false);
                  deletePlaylist(playlist.id);
                }}
              >
                {tr("delete", undefined, "Delete")}
              </Button>
            </>
          }
        >
          <p className="p-5 text-xs text-[var(--color-muted)]">
            {tr(
              "playlist_delete_body",
              undefined,
              "This removes the playlist and all its steps. The payload files on disk are not touched.",
            )}
          </p>
        </Modal>
      )}

      {playlist.steps.length === 0 ? (
        <div className="rounded border border-dashed border-[var(--color-border)] p-3 text-center text-xs text-[var(--color-muted)]">
          {tr(
            "playlist_no_steps",
            undefined,
            "No steps yet. Add a payload below.",
          )}
        </div>
      ) : (
        <ol className="grid gap-1.5">
          {playlist.steps.map((step, i) => (
            <li
              key={`${step.path}-${i}`}
              // sm:flex-wrap (not just flex-row): at a large Text size the
              // fixed-width ip/port/sleep controls grow and used to crush the
              // step path to 0px on one nowrap line. Allowing the row to wrap
              // — with a floored path min-width below — drops the controls to
              // their own line instead, keeping the payload path visible.
              className={`flex flex-col gap-2 rounded border p-2 text-xs sm:flex-row sm:flex-wrap sm:items-center ${
                i === activeStepIndex
                  ? "border-[var(--color-accent)] bg-[var(--color-surface-2)]"
                  : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
              }`}
            >
              {/* Status + index + path. On phones this is its own line so
                  the fixed-width controls below can wrap underneath instead
                  of pushing the row off-screen. */}
              <div className="flex min-w-0 items-center gap-2 sm:min-w-[12rem] sm:flex-1">
                <StepStatusIcon
                  index={i}
                  activeIndex={activeStepIndex}
                  isRunning={isThisRunning}
                />
                <span className="shrink-0 font-mono text-xs text-[var(--color-muted)] tabular-nums">
                  {String(i + 1).padStart(2, "0")}
                </span>
                <div className="min-w-0 flex-1 truncate font-mono text-xs">
                  {step.path}
                </div>
              </div>
              {/* Per-step ip/port/sleep overrides + reorder/remove. Wraps on
                  narrow viewports; single trailing row on sm+. */}
              <div className="flex flex-wrap items-center gap-2 sm:justify-end">
                {/* Per-step IP override. Empty = use the playlist-wide
                  IP entered above. Useful for sequences targeting
                  multiple PS5s (push a loader to dev kit, push
                  harness to test kit). */}
                <label className="flex shrink-0 items-center gap-1 text-xs text-[var(--color-muted)]">
                  {tr("playlist_step_ip", undefined, "ip")}
                  <input
                    type="text"
                    value={step.ip ?? ""}
                    placeholder={tr(
                      "playlist_step_ip_placeholder",
                      undefined,
                      "default",
                    )}
                    onChange={(e) =>
                      updateStep(playlist.id, i, {
                        ip: e.target.value.trim(),
                      })
                    }
                    disabled={anyRunning}
                    className="w-28 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-xs"
                  />
                </label>
                {/* Per-step port override. Empty / 0 = use the
                  playlist-wide port (default 9021). */}
                <label className="flex shrink-0 items-center gap-1 text-xs text-[var(--color-muted)]">
                  {tr("playlist_step_port", undefined, "port")}
                  <input
                    type="number"
                    min={0}
                    max={65535}
                    step={1}
                    value={step.port ?? ""}
                    placeholder={tr(
                      "playlist_step_port_placeholder",
                      undefined,
                      "default",
                    )}
                    onChange={(e) => {
                      const raw = e.target.value.trim();
                      const n =
                        raw === "" ? 0 : Math.max(0, Math.floor(Number(raw)));
                      updateStep(playlist.id, i, {
                        port: Number.isFinite(n) && n > 0 ? n : undefined,
                      });
                    }}
                    disabled={anyRunning}
                    className="w-16 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-right text-xs tabular-nums"
                  />
                </label>
                <label className="flex shrink-0 items-center gap-1 text-xs text-[var(--color-muted)]">
                  {tr("playlist_step_sleep", undefined, "sleep")}
                  <input
                    type="number"
                    min={0}
                    step={100}
                    value={step.sleepMs}
                    onChange={(e) =>
                      updateStep(playlist.id, i, {
                        sleepMs: sanitiseSleepMs(e.target.value),
                      })
                    }
                    disabled={anyRunning}
                    className="w-16 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-right text-xs tabular-nums"
                  />
                  <span>ms</span>
                </label>
                <div className="flex shrink-0 items-center gap-0.5">
                  <button
                    type="button"
                    onClick={() => moveStepUp(playlist.id, i)}
                    disabled={anyRunning || i === 0}
                    className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                    title={tr("playlist_step_up", undefined, "Move up")}
                  >
                    <ArrowUp size={12} />
                  </button>
                  <button
                    type="button"
                    onClick={() => moveStepDown(playlist.id, i)}
                    disabled={anyRunning || i === playlist.steps.length - 1}
                    className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] disabled:opacity-30"
                    title={tr("playlist_step_down", undefined, "Move down")}
                  >
                    <ArrowDown size={12} />
                  </button>
                  <button
                    type="button"
                    onClick={() => removeStep(playlist.id, i)}
                    disabled={anyRunning}
                    className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-bad)] hover:text-[var(--color-accent-contrast)] disabled:opacity-30"
                    title={tr("playlist_step_remove", undefined, "Remove step")}
                  >
                    <X size={12} />
                  </button>
                </div>
              </div>
            </li>
          ))}
        </ol>
      )}

      <div className="mt-2 flex justify-end">
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<FolderOpen size={12} />}
          onClick={handleAddStep}
          disabled={anyRunning}
        >
          {tr("playlist_add_step", undefined, "Add step")}
        </Button>
      </div>
    </article>
  );
}

function StepStatusIcon({
  index,
  activeIndex,
  isRunning,
}: {
  index: number;
  activeIndex: number;
  isRunning: boolean;
}) {
  if (!isRunning || activeIndex < 0) {
    return (
      <CircleDashed size={14} className="shrink-0 text-[var(--color-muted)]" />
    );
  }
  if (index < activeIndex) {
    return (
      <CheckCircle2 size={14} className="shrink-0 text-[var(--color-good)]" />
    );
  }
  if (index === activeIndex) {
    return (
      <Loader2
        size={14}
        className="shrink-0 animate-spin text-[var(--color-accent)]"
      />
    );
  }
  return (
    <CircleDashed size={14} className="shrink-0 text-[var(--color-muted)]" />
  );
}

function basename(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/** Compact "just now / 2m ago / 3h ago / Apr 18" for the recently-run
 *  strip. Mirrors the SendPanel history formatter; coarse enough that a
 *  stale render between ticks reads fine. */
function formatAgo(ms: number): string {
  if (ms <= 0) return "";
  const diff = Date.now() - ms;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  if (diff < 7 * 86_400_000) return `${Math.floor(diff / 86_400_000)}d ago`;
  return new Date(ms).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
  });
}
