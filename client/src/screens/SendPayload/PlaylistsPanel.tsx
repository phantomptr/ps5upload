import { useEffect, useRef, useState } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  ArrowDown,
  ArrowUp,
  CheckCircle2,
  CircleDashed,
  Clock,
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
} from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { usePayloadPlaylistsStore } from "../../state/payloadPlaylists";
import { sanitiseSleepMs, type Playlist } from "../../lib/playlistOps";

/**
 * Playlist editor + runner panel for the SendPayload screen.
 *
 * Each playlist is an ordered list of (payload_path, sleep_ms_after)
 * steps. Users create playlists, add steps via the same file picker
 * the main form uses, and click Run to send each payload in sequence
 * with the configured sleep between steps. The store handles
 * persistence + cancellation; this component is purely the UI.
 */
export function PlaylistsPanel({
  host,
  port,
}: {
  host: string;
  port: number;
}) {
  const tr = useTr();
  const playlists = usePayloadPlaylistsStore((s) => s.playlists);
  const loaded = usePayloadPlaylistsStore((s) => s.loaded);
  const runStatus = usePayloadPlaylistsStore((s) => s.runStatus);
  const hydrate = usePayloadPlaylistsStore((s) => s.hydrate);
  const createPlaylist = usePayloadPlaylistsStore((s) => s.createPlaylist);
  const stop = usePayloadPlaylistsStore((s) => s.stop);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

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
              onClick={stop}
            >
              {tr("playlist_stop", undefined, "Stop run")}
            </Button>
          )}
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

      <RunStatusBanner />

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
        {playlists.map((p) => (
          <PlaylistCard
            key={p.id}
            playlist={p}
            host={host}
            port={port}
            anyRunning={isBusy}
          />
        ))}
      </div>
    </section>
  );
}

function RunStatusBanner() {
  const tr = useTr();
  const runStatus = usePayloadPlaylistsStore((s) => s.runStatus);
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
    (p) =>
      "playlistId" in runStatus && p.id === runStatus.playlistId,
  );
  const name = playlist?.name ?? "playlist";

  if (runStatus.kind === "running") {
    const step = playlist?.steps[runStatus.stepIndex];
    return (
      <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface)] p-2 text-xs">
        <Loader2 size={14} className="animate-spin text-[var(--color-accent)]" />
        <span className="font-medium">{name}</span>
        <span className="text-[var(--color-muted)]">
          {tr(
            "playlist_status_running",
            { index: runStatus.stepIndex + 1, total: playlist?.steps.length ?? 0 },
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
          <ul className="mt-1.5 space-y-0.5 pl-6 font-mono text-[10px] text-[var(--color-warn)]">
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
}: {
  playlist: Playlist;
  host: string;
  port: number;
  anyRunning: boolean;
}) {
  const tr = useTr();
  const renamePlaylist = usePayloadPlaylistsStore((s) => s.renamePlaylist);
  const deletePlaylist = usePayloadPlaylistsStore((s) => s.deletePlaylist);
  const setContinueOnFailure = usePayloadPlaylistsStore(
    (s) => s.setContinueOnFailure,
  );
  const addStep = usePayloadPlaylistsStore((s) => s.addStep);
  const removeStep = usePayloadPlaylistsStore((s) => s.removeStep);
  const moveStepUp = usePayloadPlaylistsStore((s) => s.moveStepUp);
  const moveStepDown = usePayloadPlaylistsStore((s) => s.moveStepDown);
  const updateStep = usePayloadPlaylistsStore((s) => s.updateStep);
  const run = usePayloadPlaylistsStore((s) => s.run);
  const runStatus = usePayloadPlaylistsStore((s) => s.runStatus);

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
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "Payload", extensions: ["elf", "bin", "js", "lua"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (typeof picked !== "string") return;
    addStep(playlist.id, { path: picked, sleepMs: 0 });
  };

  const canRun =
    playlist.steps.length > 0 && !!host?.trim() && !anyRunning;

  return (
    <article className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-4">
      <header className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
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
          <span className="text-[11px] text-[var(--color-muted)]">
            ·{" "}
            {tr(
              "playlist_step_count",
              { count: playlist.steps.length },
              `${playlist.steps.length} step${playlist.steps.length === 1 ? "" : "s"}`,
            )}
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex items-center gap-1.5 text-[11px] text-[var(--color-muted)]">
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
                  ? tr("playlist_need_steps", undefined, "Add at least one step")
                  : tr(
                      "playlist_run_tooltip",
                      { name: playlist.name },
                      "Run \"{name}\"",
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
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
          onClick={() => setConfirmingDelete(false)}
        >
          <div
            className="w-full max-w-md rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-5"
            onClick={(e) => e.stopPropagation()}
          >
            <h3 className="mb-2 text-sm font-semibold">
              {tr(
                "playlist_delete_title",
                { name: playlist.name },
                "Delete \"{name}\"?",
              )}
            </h3>
            <p className="mb-4 text-xs text-[var(--color-muted)]">
              {tr(
                "playlist_delete_body",
                undefined,
                "This removes the playlist and all its steps. The payload files on disk are not touched.",
              )}
            </p>
            <div className="flex items-center justify-end gap-2">
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
            </div>
          </div>
        </div>
      )}

      {playlist.steps.length === 0 ? (
        <div className="rounded border border-dashed border-[var(--color-border)] p-3 text-center text-[11px] text-[var(--color-muted)]">
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
              className={`flex items-center gap-2 rounded border p-2 text-xs ${
                i === activeStepIndex
                  ? "border-[var(--color-accent)] bg-[var(--color-surface-2)]"
                  : "border-[var(--color-border)] bg-[var(--color-surface-2)]"
              }`}
            >
              <StepStatusIcon
                index={i}
                activeIndex={activeStepIndex}
                isRunning={isThisRunning}
              />
              <span className="font-mono text-[11px] text-[var(--color-muted)] tabular-nums">
                {String(i + 1).padStart(2, "0")}
              </span>
              <div className="min-w-0 flex-1 truncate font-mono text-[11px]">
                {step.path}
              </div>
              {/* Per-step IP override. Empty = use the playlist-wide
                  IP entered above. Useful for sequences targeting
                  multiple PS5s (push GoldHEN to dev kit, push
                  harness to test kit). */}
              <label className="flex shrink-0 items-center gap-1 text-[10px] text-[var(--color-muted)]">
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
                  className="w-28 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-[11px]"
                />
              </label>
              {/* Per-step port override. Empty / 0 = use the
                  playlist-wide port (default 9021). */}
              <label className="flex shrink-0 items-center gap-1 text-[10px] text-[var(--color-muted)]">
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
                    const n = raw === "" ? 0 : Math.max(0, Math.floor(Number(raw)));
                    updateStep(playlist.id, i, {
                      port: Number.isFinite(n) && n > 0 ? n : undefined,
                    });
                  }}
                  disabled={anyRunning}
                  className="w-16 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-right text-[11px] tabular-nums"
                />
              </label>
              <label className="flex shrink-0 items-center gap-1 text-[10px] text-[var(--color-muted)]">
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
                  className="w-16 rounded border border-[var(--color-border)] bg-[var(--color-surface)] px-1 py-0.5 text-right text-[11px] tabular-nums"
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
      <Loader2 size={14} className="shrink-0 animate-spin text-[var(--color-accent)]" />
    );
  }
  return <CircleDashed size={14} className="shrink-0 text-[var(--color-muted)]" />;
}

function basename(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}
