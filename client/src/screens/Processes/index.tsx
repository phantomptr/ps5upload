import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Cpu,
  RefreshCw,
  RotateCw,
  Skull,
  TriangleAlert,
} from "lucide-react";

import { useConnectionStore } from "../../state/connection";
import { mgmtAddr, transferAddr } from "../../lib/addr";
import { useDocumentVisible } from "../../lib/visibility";
import {
  PageHeader,
  Button,
  EmptyState,
  ErrorCard,
  Modal,
} from "../../components";
import { GameIcon } from "../../components/GameIcon";
import { PlatformBadge } from "../../components/PlatformBadge";
import { platformForTitleId } from "../../lib/titleDetails";
import { useTr } from "../../state/lang";
import {
  appLaunch,
  processKill,
  processList,
  type ProcessInfo,
} from "../../api/ps5";
import { log } from "../../state/logs";

/**
 * Process manager — a live task-manager for the connected PS5.
 *
 * Lists every running process (pid, memory, threads) with a `kind`
 * classification computed payload-side. The default view shows only user
 * processes (games + .elf payloads); a toggle reveals system processes,
 * and killing anything classified "system" requires an extra confirm —
 * killing SceShellUI/SceShellCore freezes the console.
 *
 * "Restart" (apps only) = kill the app then relaunch it by title id via the
 * existing app-launch path, so there's one launch code path.
 */

// 3s, not 2s: a process list rarely changes faster than the eye cares about,
// and this poll shares the single-client PS5 mgmt port with status polling +
// uploads/installs. Combined with the visibility gate + in-flight guard below,
// it keeps idle mgmt traffic modest.
const REFRESH_MS = 3000;
/** Delay between kill and relaunch on a Restart, so the OS has settled the
 *  old process before the launcher fires. */
const RESTART_RELAUNCH_DELAY_MS = 1200;

type SortKey = "memory" | "pid" | "name" | "threads";

export default function ProcessesScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = mgmtAddr(host);
  const online = payloadStatus === "up";
  // Pause polling when the window is hidden/minimized — no point hammering the
  // mgmt port for a view nobody is looking at (matches AppShell's pollers).
  const windowVisible = useDocumentVisible();

  const [procs, setProcs] = useState<ProcessInfo[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loadedOnce, setLoadedOnce] = useState(false);
  const [showSystem, setShowSystem] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(true);
  const [sortKey, setSortKey] = useState<SortKey>("memory");
  const [busyPid, setBusyPid] = useState<number | null>(null);
  // A process the user asked to kill that's classified "system" — held here
  // to drive the are-you-sure modal before the kill actually fires.
  const [confirmKill, setConfirmKill] = useState<ProcessInfo | null>(null);

  // Guard against a slow refresh landing after the user navigated away or a
  // newer refresh already resolved (last-write-wins by generation).
  const genRef = useRef(0);
  // At most one process-list probe in flight at a time: on a busy mgmt port a
  // round-trip can exceed the poll interval, and without this the 3s timer
  // would stack probes on the single-client socket.
  const inFlightRef = useRef(false);
  // Last fetch-error string we logged, so a persistent failure logs once (on
  // the transition) rather than every poll — keeps the bug bundle clean.
  const lastLoggedErrRef = useRef<string | null>(null);
  // Track mount so a relaunch scheduled by Restart doesn't touch state after
  // the user navigated away.
  const mountedRef = useRef(true);
  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    if (!online || inFlightRef.current) return;
    inFlightRef.current = true;
    const gen = ++genRef.current;
    try {
      const res = await processList(addr);
      if (gen !== genRef.current) return;
      setProcs(res.processes);
      setTruncated(res.truncated);
      setError(null);
      setLoadedOnce(true);
      lastLoggedErrRef.current = null;
    } catch (e) {
      if (gen !== genRef.current) return;
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setLoadedOnce(true);
      // Log once per distinct error (not every 3s) so a dropped helper leaves
      // a trace in bug reports without spamming the log.
      if (lastLoggedErrRef.current !== msg) {
        lastLoggedErrRef.current = msg;
        log.warn("process", `process list fetch failed: ${msg}`);
      }
    } finally {
      inFlightRef.current = false;
    }
  }, [addr, online]);

  // Initial load + auto-refresh poll. Gated on window visibility (don't poll a
  // hidden window) and the auto-refresh toggle. Pausing keeps the last
  // snapshot; the manual button still works regardless.
  useEffect(() => {
    if (!online || !windowVisible) return;
    void refresh();
    if (!autoRefresh) return;
    const id = window.setInterval(() => void refresh(), REFRESH_MS);
    return () => window.clearInterval(id);
  }, [refresh, autoRefresh, online, windowVisible]);

  const doKill = useCallback(
    async (p: ProcessInfo) => {
      setBusyPid(p.pid);
      try {
        await processKill(addr, p.pid);
        log.info("process", `killed ${p.comm || p.name} (pid ${p.pid})`);
        await refresh();
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setError(msg);
        // Log the failure too — success is logged above, so without this a bug
        // report would show "killed X" but never "kill X failed".
        log.warn(
          "process",
          `kill pid ${p.pid} (${p.comm || p.name}) failed: ${msg}`,
        );
      } finally {
        setBusyPid(null);
      }
    },
    [addr, refresh],
  );

  // Kill request: "system" AND "app" (game) kills detour through the confirm
  // modal — system because it can crash the console, app because killing a
  // running game loses unsaved progress. "payload" homebrew is cheap +
  // restartable, so it's killed immediately.
  const requestKill = useCallback(
    (p: ProcessInfo) => {
      // The helper's own process can't be killed (the payload guards it); the
      // button is disabled, but guard here too so nothing slips through.
      if (p.is_self) return;
      if (p.kind === "system" || p.kind === "app") setConfirmKill(p);
      else void doKill(p);
    },
    [doKill],
  );

  const doRestart = useCallback(
    async (p: ProcessInfo) => {
      if (!p.title_id) return;
      setBusyPid(p.pid);
      try {
        await processKill(addr, p.pid);
        log.info(
          "process",
          `restart: killed ${p.comm || p.name} (pid ${p.pid}), relaunching ${p.title_id}`,
        );
        // Relaunch by title id after a short settle. Hold busyPid across the
        // whole kill→settle→relaunch so the row can't be re-actioned mid-flight,
        // and bail if the screen unmounted during the settle.
        await new Promise((r) =>
          window.setTimeout(r, RESTART_RELAUNCH_DELAY_MS),
        );
        if (!mountedRef.current) return;
        try {
          await appLaunch(transferAddr(host), p.title_id);
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          log.warn("process", `relaunch ${p.title_id} failed: ${msg}`);
          if (mountedRef.current) setError(msg);
        }
        await refresh();
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        setError(msg);
        log.warn(
          "process",
          `restart pid ${p.pid} (${p.title_id}) failed at kill: ${msg}`,
        );
      } finally {
        if (mountedRef.current) setBusyPid(null);
      }
    },
    [addr, host, refresh],
  );

  const visible = useMemo(() => {
    const filtered = showSystem
      ? procs
      : procs.filter((p) => p.kind !== "system");
    const sorted = [...filtered].sort((a, b) => {
      switch (sortKey) {
        case "pid":
          return a.pid - b.pid;
        case "name":
          return (a.comm || a.name).localeCompare(b.comm || b.name);
        case "threads":
          return b.threads - a.threads;
        case "memory":
        default:
          return b.memory_mib - a.memory_mib;
      }
    });
    return sorted;
  }, [procs, showSystem, sortKey]);

  const systemCount = procs.filter((p) => p.kind === "system").length;

  if (!online) {
    return (
      <div className="mx-auto max-w-5xl p-6">
        <PageHeader
          icon={Cpu}
          title={tr("processes_title", undefined, "Processes")}
        />
        <EmptyState
          icon={Cpu}
          size="hero"
          title={tr(
            "processes_offline_title",
            undefined,
            "PS5 helper not running",
          )}
          message={tr(
            "processes_offline_desc",
            undefined,
            "Connect to a PS5 and load the helper payload to manage processes.",
          )}
        />
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-5xl p-6">
      <PageHeader
        icon={Cpu}
        title={tr("processes_title", undefined, "Processes")}
        count={procs.length || undefined}
        description={tr(
          "processes_subtitle",
          undefined,
          "Live process list for the connected PS5. Kill or restart processes.",
        )}
      />

      <div className="mb-4 flex flex-wrap items-center gap-3">
        <label className="flex items-center gap-1.5 text-xs">
          <input
            type="checkbox"
            checked={showSystem}
            onChange={(e) => setShowSystem(e.target.checked)}
            className="h-3.5 w-3.5"
          />
          {tr("processes_show_system", undefined, "Show system processes")}
          {systemCount > 0 && (
            <span className="text-[var(--color-muted)]">({systemCount})</span>
          )}
        </label>
        <label className="flex items-center gap-1.5 text-xs">
          <input
            type="checkbox"
            checked={autoRefresh}
            onChange={(e) => setAutoRefresh(e.target.checked)}
            className="h-3.5 w-3.5"
          />
          {tr("processes_auto_refresh", undefined, "Auto-refresh")}
        </label>
        <div className="ml-auto flex items-center gap-2">
          <select
            value={sortKey}
            onChange={(e) => setSortKey(e.target.value as SortKey)}
            className="rounded border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 text-xs"
          >
            <option value="memory">
              {tr("processes_sort_memory", undefined, "Sort: Memory")}
            </option>
            <option value="threads">
              {tr("processes_sort_threads", undefined, "Sort: Threads")}
            </option>
            <option value="pid">
              {tr("processes_sort_pid", undefined, "Sort: PID")}
            </option>
            <option value="name">
              {tr("processes_sort_name", undefined, "Sort: Name")}
            </option>
          </select>
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RefreshCw size={12} />}
            onClick={() => void refresh()}
          >
            {tr("refresh", undefined, "Refresh")}
          </Button>
        </div>
      </div>

      {error && (
        <ErrorCard
          title={tr("processes_error", undefined, "Process action failed")}
          detail={error}
          onDismiss={() => setError(null)}
        />
      )}

      {truncated && (
        <div className="mb-3 flex items-center gap-2 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-warn)]">
          <TriangleAlert size={14} />
          {tr(
            "processes_truncated",
            undefined,
            "The process list was cut short — too many processes to show all.",
          )}
        </div>
      )}

      {loadedOnce && visible.length === 0 && !error ? (
        <EmptyState
          icon={Cpu}
          title={tr("processes_empty_title", undefined, "No processes")}
          message={
            showSystem
              ? tr("processes_empty_all", undefined, "Nothing is running.")
              : tr(
                  "processes_empty_user",
                  undefined,
                  "No games or payloads are running. Toggle “Show system processes” to see everything.",
                )
          }
        />
      ) : (
        <ul className="grid gap-1.5">
          {visible.map((p) => (
            <ProcessRow
              key={p.pid}
              proc={p}
              host={host}
              busy={busyPid === p.pid}
              onKill={requestKill}
              onRestart={doRestart}
            />
          ))}
        </ul>
      )}

      {confirmKill && (
        <Modal
          open
          onClose={() => setConfirmKill(null)}
          role="alertdialog"
          size="md"
          title={
            confirmKill.kind === "app"
              ? tr(
                  "processes_kill_app_title",
                  { name: confirmKill.comm || confirmKill.name },
                  'Close "{name}"?',
                )
              : tr(
                  "processes_kill_system_title",
                  { name: confirmKill.comm || confirmKill.name },
                  'Kill system process "{name}"?',
                )
          }
          footer={
            <>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => setConfirmKill(null)}
              >
                {tr("cancel", undefined, "Cancel")}
              </Button>
              <Button
                variant="danger"
                size="sm"
                leftIcon={<Skull size={12} />}
                onClick={() => {
                  const p = confirmKill;
                  setConfirmKill(null);
                  void doKill(p);
                }}
              >
                {tr("processes_kill", undefined, "Kill")}
              </Button>
            </>
          }
        >
          <p className="p-5 text-xs text-[var(--color-muted)]">
            {confirmKill.kind === "app"
              ? tr(
                  "processes_kill_app_body",
                  { name: confirmKill.comm || confirmKill.name },
                  'This force-closes the running game "{name}". Any unsaved progress will be lost. Use Restart instead to relaunch it.',
                )
              : tr(
                  "processes_kill_system_body",
                  { name: confirmKill.comm || confirmKill.name },
                  'This is a PS5 system process. Killing "{name}" may freeze or crash the console, forcing a reboot. Only continue if you know what you are doing.',
                )}
          </p>
        </Modal>
      )}
    </div>
  );
}

/** Memoized so the 3s poll's fresh array doesn't re-render every row — only
 *  rows whose meaningful data (memory/threads/kind/title/busy) actually
 *  changed. Callbacks are stable (useCallback in the parent) and take the
 *  proc, so identity holds across polls. */
const ProcessRow = memo(
  ProcessRowImpl,
  (a, b) =>
    a.busy === b.busy &&
    a.host === b.host &&
    a.onKill === b.onKill &&
    a.onRestart === b.onRestart &&
    a.proc.pid === b.proc.pid &&
    a.proc.kind === b.proc.kind &&
    a.proc.is_self === b.proc.is_self &&
    a.proc.title_id === b.proc.title_id &&
    a.proc.threads === b.proc.threads &&
    a.proc.memory_mib === b.proc.memory_mib &&
    (a.proc.comm || a.proc.name) === (b.proc.comm || b.proc.name),
);

function ProcessRowImpl({
  proc,
  host,
  busy,
  onKill,
  onRestart,
}: {
  proc: ProcessInfo;
  host: string;
  busy: boolean;
  onKill: (p: ProcessInfo) => void;
  onRestart: (p: ProcessInfo) => void;
}) {
  const tr = useTr();
  const platform = platformForTitleId(proc.title_id);
  const isApp = proc.kind === "app";
  const isSelf = !!proc.is_self;
  const label = proc.comm || proc.name;

  return (
    // flex-wrap + a floored identity block: at large OS text size or a narrow
    // width the Restart/Kill buttons drop to their own line instead of crushing
    // the process name to zero. The icon + identity stay together on line 1.
    <li className="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-sm">
      {isApp ? (
        <GameIcon host={host} size={36} titleId={proc.title_id || null} />
      ) : (
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-[var(--color-surface-3)]">
          <Cpu size={16} className="text-[var(--color-muted)]" />
        </div>
      )}
      <div className="min-w-[min(100%,11rem)] flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <span className="truncate font-medium">{label}</span>
          <KindBadge kind={proc.kind} />
          {platform && <PlatformBadge platform={platform} />}
          {isSelf && (
            <span className="shrink-0 rounded-full border border-[var(--color-accent)] px-1.5 py-px text-[10px] font-medium text-[var(--color-accent)]">
              {tr("processes_this_tool", undefined, "this tool")}
            </span>
          )}
        </div>
        <div className="mt-0.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 font-mono text-xs text-[var(--color-muted)] tabular-nums">
          <span>{tr("processes_pid", { pid: proc.pid }, "pid {pid}")}</span>
          <span>
            {tr("processes_mb", { mb: proc.memory_mib.toFixed(1) }, "{mb} MB")}
          </span>
          <span>
            {tr("processes_threads", { n: proc.threads }, "{n} threads")}
          </span>
          {proc.title_id && <span>{proc.title_id}</span>}
        </div>
      </div>
      <div className="ml-auto flex shrink-0 items-center gap-1.5">
        {isApp && proc.title_id && (
          <Button
            variant="secondary"
            size="sm"
            leftIcon={<RotateCw size={12} />}
            onClick={() => onRestart(proc)}
            disabled={busy}
            title={tr("processes_restart_tooltip", undefined, "Kill and relaunch")}
          >
            {tr("processes_restart", undefined, "Restart")}
          </Button>
        )}
        <Button
          variant="ghost"
          size="sm"
          leftIcon={<Skull size={12} />}
          onClick={() => onKill(proc)}
          disabled={busy || isSelf}
          title={
            isSelf
              ? tr(
                  "processes_kill_self_tooltip",
                  undefined,
                  "This is the PS5Upload helper — killing it would disconnect the tool.",
                )
              : tr("processes_kill_tooltip", undefined, "Send SIGKILL")
          }
        >
          {tr("processes_kill", undefined, "Kill")}
        </Button>
      </div>
    </li>
  );
}

function KindBadge({ kind }: { kind: ProcessInfo["kind"] }) {
  const tr = useTr();
  const map: Record<ProcessInfo["kind"], { label: string; cls: string }> = {
    app: {
      label: tr("processes_kind_app", undefined, "Game"),
      cls: "border-[var(--color-ps5)] text-[var(--color-ps5)]",
    },
    payload: {
      label: tr("processes_kind_payload", undefined, "Payload"),
      cls: "border-[var(--color-accent)] text-[var(--color-accent)]",
    },
    system: {
      label: tr("processes_kind_system", undefined, "System"),
      cls: "border-[var(--color-muted)] text-[var(--color-muted)]",
    },
  };
  const m = map[kind] ?? map.system;
  return (
    <span
      className={`shrink-0 rounded-full border px-1.5 py-px text-[10px] font-medium ${m.cls}`}
    >
      {m.label}
    </span>
  );
}
