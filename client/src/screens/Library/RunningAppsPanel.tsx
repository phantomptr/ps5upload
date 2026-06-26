import { useCallback, useEffect, useRef, useState } from "react";
import {
  Activity,
  RefreshCw,
  Loader2,
  Pause,
  Play,
  X,
  AlertTriangle,
} from "lucide-react";
import {
  appListRunning,
  appdbQuery,
  appSuspend,
  appResume,
  appKill,
  type AppDbEntry,
  type RunningApp,
} from "../../api/ps5";
import { Button } from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";
import { useRunningAppsStore } from "../../state/runningApps";
import { useDocumentVisible } from "../../lib/visibility";
import { transferScreenBusy } from "../../lib/ps5Transfers";

interface RunningJoined {
  app_id: number;
  /** Resolved from app.db when found; null when the running app isn't
   *  in app.db (rare — system processes, ephemeral installers). */
  title_id: string | null;
  name: string | null;
}

/**
 * Live "running apps" panel. Joins Phase 18's app_list_running with
 * Phase 33's appdb_query so each running app gets a friendly name +
 * title id alongside its app_id, plus suspend/resume/kill buttons.
 *
 * Polls every 5s while mounted. The app.db lookup is cached for
 * 30s — title metadata barely changes between probes and the
 * sqlite walk is the more expensive of the two RPCs.
 */
export default function RunningAppsPanel({ mgmtAddr }: { mgmtAddr: string }) {
  const tr = useTr();
  const [apps, setApps] = useState<RunningJoined[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busyApp, setBusyApp] = useState<number | null>(null);
  const { confirm: confirmDialog, dialog: confirmDialogNode } = useConfirm();
  // Cache lives in a ref, not state. With state, every 30s cache
  // refresh would change `fetchAppdb`'s identity, then `refresh`'s,
  // then tear down + recreate the 5s setInterval and immediately
  // call refresh() again — duplicate fetch every 30s + needless
  // re-renders. The cache is read inside async functions, so React
  // doesn't need to observe it.
  const appdbCacheRef = useRef<{
    addr: string;
    fetched_ms: number;
    entries: AppDbEntry[];
  } | null>(null);

  const fetchAppdb = useCallback(async (): Promise<AppDbEntry[]> => {
    const cached = appdbCacheRef.current;
    // Key the cache by console address: after a console switch the panel stays
    // mounted with a new mgmtAddr, so a time-only check would join the new
    // console's running list against the previous console's app.db — showing
    // the wrong title names/IDs (app_id values are small and collide across
    // consoles).
    if (
      cached &&
      cached.addr === mgmtAddr &&
      Date.now() - cached.fetched_ms < 30_000
    ) {
      return cached.entries;
    }
    try {
      const r = await appdbQuery(mgmtAddr);
      appdbCacheRef.current = {
        addr: mgmtAddr,
        fetched_ms: Date.now(),
        entries: r.apps,
      };
      return r.apps;
    } catch {
      // Only fall back to stale entries if they belong to THIS console.
      return cached?.addr === mgmtAddr ? cached.entries : [];
    }
  }, [mgmtAddr]);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [running, db] = await Promise.all([
        appListRunning(mgmtAddr),
        fetchAppdb(),
      ]);
      const dbByAppId = new Map<number, AppDbEntry>();
      for (const e of db) dbByAppId.set(e.app_id, e);
      const list: RunningApp[] = running.apps ?? [];
      const joined: RunningJoined[] = list.map((r) => {
        const hit = dbByAppId.get(r.app_id);
        return {
          app_id: r.app_id,
          title_id: hit?.title_id ?? null,
          name: hit?.name ?? null,
        };
      });
      setApps(joined);
      // Publish title IDs into the shared store so Library rows can
      // render their "running" badge without each one polling
      // separately. Tag with the address so a profile switch in the
      // PS5 roster can clear stale data tied to a different console.
      useRunningAppsStore.getState().setRunning(
        joined.map((j) => j.title_id).filter((t): t is string => !!t),
        mgmtAddr,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [mgmtAddr, fetchAppdb]);

  // Pause polling while the window is hidden — `RunningAppsPanel`
  // also publishes into the shared store consumed by the play-time
  // accumulator, so a long minimization would otherwise keep
  // hammering the PS5 mgmt port + double-credit play-time when the
  // window finally comes back. Mirror's the AppShell pattern.
  // Clear the displayed running-apps the instant the target console changes.
  // The panel stays mounted across a console switch, and refresh() takes
  // ~50-500ms to resolve against the new console — without this reset the old
  // console's rows (and their Kill/Suspend/Resume buttons) stay live, and a
  // click would dispatch the action to the NEW console's mgmtAddr against the
  // OLD console's app_id (app_id values are small and collide across
  // consoles). Emptying apps collapses the panel until the new data lands.
  useEffect(() => {
    setApps([]);
    setError(null);
  }, [mgmtAddr]);

  const visible = useDocumentVisible();
  useEffect(() => {
    if (!visible) return;
    refresh();
    const id = window.setInterval(() => {
      // Skip the auto-poll while an upload to this console is running: the 5s
      // appListRunning + appdb query hit the same mgmt port a folder reconcile
      // is bursting per-file finalize calls on, and the contention can collapse
      // a many-file upload's throughput. Resumes the moment the upload ends.
      if (transferScreenBusy(mgmtAddr)) return;
      refresh();
    }, 5000);
    return () => window.clearInterval(id);
  }, [refresh, visible, mgmtAddr]);

  async function doAction(
    appId: number,
    label: string,
    fn: (addr: string, id: number) => Promise<{ ok: boolean; err?: string }>,
  ) {
    if (busyApp !== null) return;
    setBusyApp(appId);
    try {
      const ack = await fn(mgmtAddr, appId);
      if (!ack.ok) {
        pushNotification(
          "warning",
          withConsolePrefix(mgmtAddr, `${label} failed`),
          {
            body: ack.err ?? "unknown error",
          },
        );
      } else {
        pushNotification(
          "info",
          withConsolePrefix(mgmtAddr, `${label} requested`),
          {
            body: `app_id ${appId}`,
          },
        );
      }
      // Re-fetch immediately so the UI reflects the new state.
      void refresh();
    } catch (e) {
      pushNotification(
        "error",
        withConsolePrefix(mgmtAddr, `${label} failed`),
        {
          body: e instanceof Error ? e.message : String(e),
        },
      );
    } finally {
      setBusyApp(null);
    }
  }

  // Hide the panel entirely when nothing is running and no error —
  // typical Library use is "I'm browsing my games", and an always-empty
  // panel would just steal screen space.
  //
  // Visibility is gated ONLY on having something to show (apps or an error),
  // never on `loading`: the panel polls every 5s, and each poll flips
  // `loading` true→false. Including `!loading` here meant that, with nothing
  // running, every tick briefly satisfied the render path (loading=true →
  // empty panel appears) then hid it again (loading=false, still empty) —
  // the Library "flickers every few seconds" bug. A poll while apps already
  // exist keeps the panel mounted (apps.length > 0) and just spins the header
  // refresh icon, so no loading indicator is lost.
  if (apps.length === 0 && !error) return null;

  return (
    <section className="mb-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      {confirmDialogNode}
      <header className="mb-2 flex items-center gap-2">
        <Activity size={14} className="text-[var(--color-good)]" />
        <h3 className="flex-1 text-sm font-semibold">
          {tr(
            "running_apps_title",
            { count: apps.length },
            `Running apps (${apps.length})`,
          )}
        </h3>
        <Button
          variant="ghost"
          size="sm"
          leftIcon={
            loading ? (
              <Loader2 size={11} className="animate-spin" />
            ) : (
              <RefreshCw size={11} />
            )
          }
          onClick={refresh}
          disabled={loading}
        >
          {tr("refresh", undefined, "Refresh")}
        </Button>
      </header>
      {error && (
        <div className="mb-2 flex items-start gap-1 text-xs text-[var(--color-bad)]">
          <AlertTriangle size={11} className="mt-0.5 shrink-0" />
          {error}
        </div>
      )}
      {apps.length === 0 ? (
        <div className="text-xs text-[var(--color-muted)]">
          {tr("running_apps_empty", undefined, "No apps currently running.")}
        </div>
      ) : (
        <ul className="space-y-1.5">
          {apps.map((a) => (
            <li
              key={a.app_id}
              className="flex items-center gap-3 rounded-md bg-[var(--color-surface)] px-2 py-1.5 text-xs"
            >
              <div className="min-w-0 flex-1">
                <div className="font-medium">
                  {a.name ?? a.title_id ?? `app ${a.app_id}`}
                </div>
                <div className="text-xs text-[var(--color-muted)]">
                  app_id {a.app_id}
                  {a.title_id && ` · ${a.title_id}`}
                </div>
              </div>
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<Pause size={11} />}
                onClick={() => doAction(a.app_id, "Suspend", appSuspend)}
                disabled={busyApp !== null}
              >
                {tr("running_apps_suspend", undefined, "Suspend")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<Play size={11} />}
                onClick={() => doAction(a.app_id, "Resume", appResume)}
                disabled={busyApp !== null}
              >
                {tr("running_apps_resume", undefined, "Resume")}
              </Button>
              <Button
                variant="ghost"
                size="sm"
                leftIcon={<X size={11} />}
                onClick={async () => {
                  const ok = await confirmDialog({
                    title: tr(
                      "running_apps_kill_confirm_title",
                      undefined,
                      "Kill app",
                    ),
                    message: tr(
                      "running_apps_kill_confirm_body",
                      { label: a.name ?? a.title_id ?? `app ${a.app_id}` },
                      `Kill ${a.name ?? a.title_id ?? `app ${a.app_id}`}? Unsaved progress will be lost.`,
                    ),
                    destructive: true,
                    confirmLabel: tr("running_apps_kill", undefined, "Kill"),
                  });
                  if (ok) void doAction(a.app_id, "Kill", appKill);
                }}
                disabled={busyApp !== null}
              >
                {tr("running_apps_kill", undefined, "Kill")}
              </Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
