// The automatic update of an older helper (lib/helperAutoUpdate), wired to the stores and the
// console. AppShell's useAutoUpdateOlderHelper asks `evaluateAutoUpdate` for each saved console
// every half minute and when a helper is seen arriving; the Console card reads the store for its
// one quiet line.
//
// The send itself is lib/ensurePayloadCurrent's: single-flight per console, never over a newer
// helper, nothing while other devices or Files are busy there, the stuck-loader check, Mesh told
// before the helper restarts, and the boot copies brought up to date after it answers (that
// asks nothing first, the same as after the Send helper button).

import { create } from "zustand";

import { fetchConsoleTasks } from "../api/consoleTasks";
import { cheatsStatus, payloadCheck, portCheck, savesOpen } from "../api/ps5";
import { consoleAddr, hostOf, PAYLOAD_MANAGER_PORT, PS5_LOADER_PORT, transferAddr } from "../lib/addr";
import { getAppVersion } from "../lib/appVersion";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { sendInFlight, sentAgoMs } from "../lib/helperSendGate";
import { launchInFlight } from "../lib/launchInFlight";
import {
  AUTO_UPDATE_SETTLE_MS,
  helperAutoUpdateDecision,
  helperAutoUpdateGate,
  OTHER_RESTART_WINDOW_MS,
  type AutoUpdateDecision,
  type AutoUpdateInput,
  type ConsoleCheck,
  type Known,
  type NeverReason,
  type WaitReason,
} from "../lib/helperAutoUpdate";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { trStatic } from "../lib/trStatic";
import { consoleBusy } from "../lib/usePoll";
import { bringUpStatusFor, useBringUpStore } from "./bringUp";
import { useConnectionStore } from "./connection";
import { consoleSettingFor } from "./consoleSettings";
import { helperSendFor, useHelperSendStore } from "./helperSend";
import { isHelperStopped } from "./helperStopped";
import { log } from "./logs";
import { meshFor, pollMesh, useMeshStore } from "./mesh";
import { useRemotePlayStore } from "./remotePlay";
import { useRosterStore } from "./roster";
import { isTerminal, useTaskStore } from "./tasks";
import { useToastStore } from "./toasts";
import { useUploadQueueStore } from "./uploadQueue";

/** What the Console card says about one console's automatic update. */
export type AutoUpdateView =
  | { state: "waiting"; why: WaitReason; to: string }
  | { state: "never"; why: NeverReason; to: string }
  | { state: "updating"; to: string }
  | { state: "failed"; to: string };

interface StoreState {
  byHost: Record<string, AutoUpdateView>;
}

export const useHelperAutoUpdateStore = create<StoreState>(() => ({ byHost: {} }));

function publish(key: string, view: AutoUpdateView | null): void {
  const cur = useHelperAutoUpdateStore.getState().byHost[key];
  if (view === null) {
    if (!cur) return;
    useHelperAutoUpdateStore.setState((s) => {
      const next = { ...s.byHost };
      delete next[key];
      return { byHost: next };
    });
    return;
  }
  if (cur && JSON.stringify(cur) === JSON.stringify(view)) return;
  useHelperAutoUpdateStore.setState((s) => ({ byHost: { ...s.byHost, [key]: view } }));
}

/** Tries per version pair (`host|from->to`) in this app run. */
const attemptsByPair = new Map<string, { n: number; at: number }>();
/** Since when each console's helper has answered, as the poller saw it. */
const upSince = new Map<string, number>();
const evaluating = new Set<string>();

/** Test seam. */
export function resetHelperAutoUpdate(): void {
  attemptsByPair.clear();
  upSince.clear();
  evaluating.clear();
  useHelperAutoUpdateStore.setState({ byHost: {} });
}

/** Keeps `upSince` in step with the poller (called on every connection store change). */
export function noteHelperStatus(now = Date.now()): void {
  const rt = useConnectionStore.getState().runtimeByHost;
  for (const [key, r] of Object.entries(rt)) {
    if (r.payloadStatus === "up") {
      if (!upSince.has(key)) upSince.set(key, now);
    } else upSince.delete(key);
  }
  for (const key of [...upSince.keys()]) if (!rt[key]) upSince.delete(key);
}

const ACTIVE = new Set(["running", "paused", "awaiting"]);

/** What this app is running on `host`, or null: transfers, the queue, Files, installs, copies,
 *  extractions, conversions and saves (the task list mirrors the engine's jobs), console-page
 *  tasks, a game being started, Remote Play and Play Together (a stream of that console),
 *  bring-up, a helper send. */
export function busyHere(host: string): string | null {
  const key = hostOf(host);
  if (transferScreenBusy(host) || consoleBusy(host)) return "a transfer";
  if (useUploadQueueStore.getState().runningHosts[key]) return "the queue";
  const task = useTaskStore
    .getState()
    .tasks.find((t) => hostOf(t.consoleId) === key && !isTerminal(t.status) && ACTIVE.has(t.status));
  if (task) return task.label || task.kind;
  if (launchInFlight(host)) return "a game start";
  const rp = useRemotePlayStore.getState();
  if (rp.stream && rp.target && hostOf(rp.target.host) === key) return "Remote Play";
  if (bringUpStatusFor(useBringUpStore.getState(), host).kind === "running") return "a bring-up";
  if (helperSendFor(useHelperSendStore.getState(), host)?.state === "busy") return "a helper send";
  return null;
}

let appVersionMemo: Promise<string | null> | null = null;
function appVersion(): Promise<string | null> {
  appVersionMemo ??= getAppVersion().catch(() => {
    appVersionMemo = null; // asked again next time
    return null;
  });
  return appVersionMemo;
}

function pairKey(key: string, from: string | null, to: string | null): string {
  return `${key}|${from ?? "?"}->${to ?? "?"}`;
}

/** Everything this app knows itself about `host`, without asking the console. */
export function localInput(host: string, app: string | null, now = Date.now()): AutoUpdateInput {
  const key = hostOf(host);
  const rt = useConnectionStore.getState().runtimeByHost[key];
  const since = upSince.get(key);
  const tries = attemptsByPair.get(pairKey(key, rt?.payloadVersion ?? null, app));
  return {
    enabled: helperAutoUpdateGate.available && consoleSettingFor(host, "auto_update_helper"),
    stopped: isHelperStopped(host),
    appVersion: app,
    helperVersion: rt?.payloadVersion ?? null,
    status: rt?.payloadStatus,
    power: rt?.power,
    pairing: rt?.session === "needs_pairing",
    upForMs: rt?.payloadStatus === "up" && since !== undefined ? now - since : null,
    busyHere: busyHere(host),
    sendInFlight: sendInFlight(host),
    sentAgoMs: sentAgoMs(host, now),
    attempts: tries?.n ?? 0,
    lastAttemptAgoMs: tries ? now - tries.at : null,
    console: null,
  };
}

/** Matches a helper's answer for a request it does not know (an older helper). */
const UNSUPPORTED = /unknown method|unsupported|not supported/i;

/** Asks the console what a restart would break, right before an automatic update. */
export async function checkConsole(host: string): Promise<ConsoleCheck> {
  const bare = hostOf(host) || host;
  const probe = await payloadCheck(host).catch(() => null);
  const reachable = !!probe?.reachable;
  if (!reachable) {
    return {
      reachable: false,
      version: null,
      othersBusy: "unknown",
      locks: "unknown",
      otherRestart: false,
      consoleTasks: "unknown",
      saveOpen: "unknown",
      cheats: "unknown",
      loaderOpen: false,
    };
  }
  // Mesh: read now, not the last poll (another device may have just started something).
  await pollMesh(host).catch(() => undefined);
  const mesh = meshFor(useMeshStore.getState(), host);
  let othersBusy: Known = "unknown";
  let locks: Known = "unknown";
  let otherRestart = false;
  if (mesh.supported === false) {
    // A helper or engine without Mesh: no other device can say it is there.
    othersBusy = "idle";
    locks = "idle";
  } else if (mesh.state && !mesh.error) {
    const st = mesh.state;
    othersBusy = st.devices.some(
      (d) => d.online && d.id !== st.you && d.activity.some((a) => a.kind !== "browsers"),
    )
      ? "busy"
      : "idle";
    locks = st.locks.length > 0 ? "busy" : "idle";
    otherRestart = st.events.some(
      (e) => e.kind === "restart" && e.by !== st.you && st.now - e.at < OTHER_RESTART_WINDOW_MS,
    );
  }
  const [consoleTasks, saveOpen, cheats, loaderOpen] = await Promise.all([
    fetchConsoleTasks(host)
      .then((l): Known => (!l.supported ? "unknown" : l.tasks.some((t) => t.state === "running") ? "busy" : "idle"))
      .catch((): Known => "unknown"),
    savesOpen(consoleAddr(host), "")
      .then((r): Known => (r.mounted ? "busy" : "idle"))
      .catch((e: unknown): Known => (UNSUPPORTED.test(String(e instanceof Error ? e.message : e)) ? "idle" : "unknown")),
    cheatsStatus(transferAddr(host))
      .then((c): Known => (c.enabled && c.game_running ? "busy" : "idle"))
      .catch((): Known => "unknown"),
    (async () =>
      (await portCheck(bare, PS5_LOADER_PORT).catch(() => false)) ||
      (await portCheck(bare, PAYLOAD_MANAGER_PORT).catch(() => false)))(),
  ]);
  return {
    reachable,
    version: probe?.payloadVersion ?? null,
    consoleBusy: !probe?.busy ? "unknown" : probe.busy.idle ? "idle" : "busy",
    othersBusy,
    locks,
    otherRestart,
    consoleTasks,
    saveOpen,
    cheats,
    loaderOpen,
  };
}

function viewOf(d: AutoUpdateDecision, to: string): AutoUpdateView | null {
  if (d.kind === "wait") return { state: "waiting", why: d.why, to };
  if (d.kind === "never") {
    // Nothing to say about a helper that matches, is newer, or can't be compared.
    return d.why === "off" || d.why === "gave-up" ? { state: "never", why: d.why, to } : null;
  }
  return null;
}

function consoleName(host: string): string | null {
  const roster = useRosterStore.getState().profiles;
  if (roster.length < 2) return null;
  return roster.find((p) => hostOf(p.host) === hostOf(host))?.name ?? null;
}

/** Decides for `host`, and updates its helper when that is safe. One evaluation per console at a
 *  time; never throws. */
export async function evaluateAutoUpdate(host: string, cancelled: () => boolean = () => false): Promise<void> {
  const key = hostOf(host);
  // Not in this build (#903): nothing is decided, shown or sent.
  if (!helperAutoUpdateGate.available) return;
  if (!key || evaluating.has(key)) return;
  evaluating.add(key);
  try {
    const app = await appVersion();
    let input = localInput(host, app);
    let d = helperAutoUpdateDecision(input);
    const to = app ?? "";
    if (d.kind === "check") {
      const c = await checkConsole(host);
      if (cancelled()) return;
      // What this app is doing may have changed while the console was asked.
      input = { ...localInput(host, app), console: c };
      d = helperAutoUpdateDecision(input);
    }
    if (d.kind !== "update") {
      publish(key, viewOf(d, to));
      return;
    }
    const from = input.console?.version ?? input.helperVersion;
    const pk = pairKey(key, input.helperVersion, app);
    const n = (attemptsByPair.get(pk)?.n ?? 0) + 1;
    attemptsByPair.set(pk, { n, at: Date.now() });
    publish(key, { state: "updating", to });
    log.info("payload", `auto-update: ${host} runs helper v${from}, older than this app's v${to}; nothing is running there, sending it (try ${n})`);
    // The console was asked just now (checkConsole): an older helper that cannot report its own
    // work may be replaced, on those answers.
    const result = await ensurePayloadCurrent(host, cancelled, false, "auto", { consoleChecked: true });
    attemptsByPair.set(pk, { n, at: Date.now() });
    if (result === "pushed") {
      attemptsByPair.delete(pk);
      publish(key, null);
      const message = trStatic("helper_auto_updated_toast", "PS5 helper updated to v{version}", { version: to });
      const name = consoleName(host);
      useToastStore.getState().push({ tone: "success", message: name ? `${name}: ${message}` : message });
      log.info("payload", `auto-update: ${host} runs helper v${to} now`);
    } else if (result === "current") {
      // Someone else updated it meanwhile: nothing of ours to say.
      attemptsByPair.delete(pk);
      publish(key, null);
    } else {
      publish(key, { state: "failed", to });
      log.warn("payload", `auto-update: the helper on ${host} was not updated (${result}); try ${n}, the next in a few minutes at most`);
    }
  } catch (e) {
    log.warn("payload", `auto-update: ${host}: ${e instanceof Error ? e.message : String(e)}`);
  } finally {
    evaluating.delete(key);
  }
}

/** How often each saved console is looked at. Cheap: the console is asked only when this app's
 *  own state already says yes and its helper is older. */
export const AUTO_UPDATE_TICK_MS = 30_000;
/** A helper seen arriving is looked at just after it has settled. */
const ARRIVAL_DELAY_MS = AUTO_UPDATE_SETTLE_MS + 2_000;

let loopUsers = 0;
let stopLoop: (() => void) | null = null;

/** Runs the automatic update for every saved console while at least one caller holds it (AppShell
 *  mounts once per console tree; one loop serves them all). Returns the release. */
export function startAutoUpdateLoop(): () => void {
  loopUsers += 1;
  if (loopUsers === 1) {
    let cancelled = false;
    const isCancelled = () => cancelled;
    const tick = () => {
      noteHelperStatus();
      for (const p of useRosterStore.getState().profiles) {
        const h = (p.host ?? "").trim();
        if (h) void evaluateAutoUpdate(h, isCancelled);
      }
    };
    // A helper seen arriving (or with a new version) is looked at once it has settled, not only
    // on the next round.
    const soon = new Map<string, ReturnType<typeof setTimeout>>();
    let last: Record<string, string> = {};
    const unsubscribe = useConnectionStore.subscribe((s) => {
      noteHelperStatus();
      const next: Record<string, string> = {};
      for (const [k, r] of Object.entries(s.runtimeByHost)) {
        if (r.payloadStatus !== "up" || !r.payloadVersion || k === "_") continue;
        next[k] = r.payloadVersion;
        if (last[k] === r.payloadVersion) continue;
        clearTimeout(soon.get(k));
        soon.set(
          k,
          setTimeout(() => {
            soon.delete(k);
            if (!cancelled) void evaluateAutoUpdate(k, isCancelled);
          }, ARRIVAL_DELAY_MS),
        );
      }
      last = next;
    });
    const id = setInterval(tick, AUTO_UPDATE_TICK_MS);
    const first = setTimeout(tick, 5_000);
    stopLoop = () => {
      cancelled = true;
      clearInterval(id);
      clearTimeout(first);
      for (const t of soon.values()) clearTimeout(t);
      unsubscribe();
    };
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    loopUsers -= 1;
    if (loopUsers === 0) {
      stopLoop?.();
      stopLoop = null;
    }
  };
}
