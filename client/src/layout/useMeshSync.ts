import { useEffect } from "react";

import { hostOf } from "../lib/addr";
import { isTauriEnv } from "../lib/tauriEnv";
import { syncId } from "../lib/meshShare";
import { consoleBusy } from "../lib/usePoll";
import { dailyBackup } from "../lib/settingsBackup";
import { getEngineUrl } from "../state/engine";
import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";
import {
  deviceName,
  loadMeshMe,
  onMeshEvent,
  othersRemotePlaying,
  pollMesh,
  presenceItems,
  publishPresence,
  RP_ACTIVITY,
  useMeshStore,
} from "../state/mesh";
import { useRemotePlayStore } from "../state/remotePlay";
import type { MeshActivityItem, MeshJoin } from "../api/mesh";
import { joinApi, presenceJoin, setOwnJoinWs } from "../screens/Play/playTogetherState";
import { useTaskStore } from "../state/tasks";
import { useToastStore } from "../state/toasts";

/** How often the selected console's devices are read while the window is shown. */
export const MESH_POLL_MS = 4_000;
/** How often this window's presence is checked (it is sent only when it changed, or every 8 s). */
export const PRESENCE_MS = 3_000;
/** The same two while data is moving to that console: every call to it then costs the transfer
 *  speed. Presence still goes out at once when the work itself changes, just not per percent. */
export const MESH_BUSY_MS = 30_000;
/** The daily settings backup is looked at this often (it runs once a day; lib/settingsBackup). */
const BACKUP_CHECK_MS = 60 * 60 * 1000;

/** What a presence list says apart from how far along each item is. */
function presenceWork(items: readonly MeshActivityItem[]): string {
  return JSON.stringify(items.map((i) => [i.kind, i.title, i.stage]));
}

/** How a backup names the app that made it: a browser of a Docker engine says so (several
 *  browsers share the engine's name). */
export function backupLabel(engineName: string | undefined): string {
  const name = engineName || "PS5Upload";
  return isTauriEnv() ? name : `${name} (browser)`;
}

/** Where a phone joins this app's Remote Play as a controller, per console (pad_link.rs):
 *  read from the engine once per console, then published with the Remote Play presence. */
const padWs = new Map<string, string>();
function padOf(h: string): { pad?: string } {
  const known = padWs.get(h);
  if (known === undefined) {
    padWs.set(h, "");
    void fetch(`${getEngineUrl()}/api/ps5/rp/pad-link?console=${encodeURIComponent(h)}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((v: { pad_ws?: string | null } | null) => padWs.set(h, v?.pad_ws ?? ""))
      .catch(() => padWs.delete(h));
  }
  return known ? { pad: known } : {};
}

/** Play Together on a console (#641): friends nearby see it and may ask to join. Read from the
 *  engine at most every 5 s, published with the Remote Play presence. */
const joinCache = new Map<string, { at: number; join: MeshJoin | null }>();
function joinOf(h: string, consoleName: string): { join?: MeshJoin } {
  const c = joinCache.get(h);
  if (!c || Date.now() - c.at > 5000) {
    joinCache.set(h, { at: Date.now(), join: c?.join ?? null });
    void joinApi.read(h).then((s) => {
      const me = useMeshStore.getState().me;
      const join = s ? presenceJoin(s, me?.name || "PS5Upload", consoleName) : null;
      joinCache.set(h, { at: Date.now(), join });
      setOwnJoinWs(join?.ws ?? null);
    });
  }
  const join = c?.join;
  return join ? { join } : {};
}

/**
 * The multi-device mesh, app-wide (8.0.0):
 *  - reads the selected console's devices, locks and events every few seconds while shown;
 *  - publishes what this window is doing on each console whose helper is up, even while hidden
 *    (an upload goes on with the window minimised, and the others should still see it);
 *  - tells the user when another device restarts the helper, takes a task over, or joins.
 * Against a helper from before 8.0.0 the reads answer "not supported" and nothing is shown.
 */
export function useMeshSync() {
  const host = useConnectionStore((s) => s.host);
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const tr = useTr();

  useEffect(() => {
    void loadMeshMe();
  }, []);

  // The daily settings backup: to the engine, and to the connected console (8.0.1). The first look
  // waits a minute, so a start-up is not slowed and the device knows its id by then.
  useEffect(() => {
    const run = () => {
      const conn = useConnectionStore.getState();
      const mesh = useMeshStore.getState();
      const key = hostOf(conn.host);
      const supported = !!key && conn.payloadStatus === "up" && mesh.byHost[key]?.supported === true;
      void dailyBackup(supported ? key : null, syncId(), backupLabel(mesh.me?.name));
    };
    const first = window.setTimeout(run, 60_000);
    const id = window.setInterval(run, BACKUP_CHECK_MS);
    return () => {
      window.clearTimeout(first);
      window.clearInterval(id);
    };
  }, []);

  // Read the selected console.
  useEffect(() => {
    const key = hostOf(host);
    if (!key || !up) return;
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let lastRead = 0;
    const tick = async () => {
      if (stopped) return;
      const shown = typeof document === "undefined" || document.visibilityState === "visible";
      if (shown && (!consoleBusy(key) || Date.now() - lastRead >= MESH_BUSY_MS)) {
        lastRead = Date.now();
        await pollMesh(key);
      }
      if (!stopped) timer = setTimeout(tick, MESH_POLL_MS);
    };
    void tick();
    const onVisible = () => {
      if (document.visibilityState === "visible") void pollMesh(key);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      stopped = true;
      if (timer) clearTimeout(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [host, up]);

  // Publish this window's work, per console it touches (and the selected one, idle or not).
  useEffect(() => {
    const sentWork = new Map<string, { work: string; at: number; items: MeshActivityItem[] }>();
    const id = window.setInterval(() => {
      const tasks = useTaskStore.getState().tasks;
      const conn = useConnectionStore.getState();
      const hosts = new Set<string>();
      for (const t of tasks) hosts.add(hostOf(t.consoleId));
      if (conn.payloadStatus === "up") hosts.add(hostOf(conn.host));
      // Remote Play from this window: the other devices see who has the console's one session.
      const rp = useRemotePlayStore.getState();
      const rpHost =
        rp.stream && rp.target && rp.phase && rp.phase !== "ended" && rp.phase !== "moved"
          ? hostOf(rp.target.host)
          : "";
      if (rpHost) hosts.add(rpHost);
      for (const h of hosts) {
        if (!h) continue;
        const rt = conn.runtimeByHost[h];
        if (rt && rt.payloadStatus !== "up") continue;
        const items = presenceItems(tasks, h);
        if (h !== rpHost && consoleBusy(h)) {
          // Only the percent moved: the same list again tells the engine this window is still here
          // and costs the console nothing; a new percent goes out now and then.
          const work = presenceWork(items);
          const sent = sentWork.get(h);
          if (sent && sent.work === work && Date.now() - sent.at < MESH_BUSY_MS) {
            void publishPresence(h, sent.items);
          } else {
            sentWork.set(h, { work, at: Date.now(), items });
            void publishPresence(h, items);
          }
          continue;
        }
        sentWork.delete(h);
        void publishPresence(
          h,
          h === rpHost ? [{ kind: RP_ACTIVITY, ...padOf(h), ...joinOf(h, rp.target?.name ?? "") }, ...items] : items,
        );
      }
    }, PRESENCE_MS);
    return () => window.clearInterval(id);
  }, []);

  // What the other devices did.
  useEffect(
    () =>
      onMeshEvent((_host, e, state) => {
        const by = state.devices.find((d) => d.id === e.by);
        const name = by ? deviceName(by) : tr("mesh_another_device", undefined, "Another device");
        const push = useToastStore.getState().push;
        switch (e.kind) {
          case "restart":
            push({
              tone: "warn",
              message: tr(
                "mesh_ev_restart",
                { name },
                `${name} is restarting the helper on this console. Transfers pause and pick up again in a moment.`,
              ),
            });
            break;
          case "takeover":
            push({ tone: "warn", message: e.text });
            break;
          case "joined":
            push({
              tone: "info",
              message: tr("mesh_ev_joined", { name }, `${name} connected to this console for the first time`),
            });
            break;
          default:
            break;
        }
      }),
    [tr],
  );

  // Another device started Remote Play on a console: say so (this window's own stream, if it
  // played that console, ends with it).
  useEffect(() => {
    const seen = new Map<string, Set<string>>();
    return useMeshStore.subscribe((s) => {
      for (const [host, m] of Object.entries(s.byHost)) {
        // Before the first answer nothing is known: what is already playing then is not news.
        if (!m.state) continue;
        const now = new Set(othersRemotePlaying(m.state).map((d) => d.id));
        const before = seen.get(host);
        seen.set(host, now);
        if (!before) continue;
        for (const id of now) {
          if (before.has(id)) continue;
          const d = m.state?.devices.find((x) => x.id === id);
          if (!d) continue;
          const name = deviceName(d);
          useToastStore.getState().push({
            tone: "info",
            message: tr("mesh_rp_started", { name }, `${name} started Remote Play on this console`),
          });
        }
      }
    });
  }, [tr]);
}
