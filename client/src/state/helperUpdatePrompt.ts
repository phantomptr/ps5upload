// "The PS5 helper is out of date" (lib/helperUpdatePrompt), wired to the stores (#941).
//
// Watches every console the status poller knows: when one answers with a helper older than this
// app, a toast and a bell entry go out once for that console and app version, and Home shows a
// notice with an Update helper button until the helper is updated or the notice is dismissed.
// Nothing is ever sent from here by itself. The button's send is the one Resend helper makes
// (the replace guard's questions, then state/helperSendRuntime, with the engine's own refusals),
// and it is not made at all while this app is in the middle of something on that console.

import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { getAppVersion } from "../lib/appVersion";
import { helperAutoUpdateGate } from "../lib/helperAutoUpdate";
import {
  helperPromptDecision,
  updateHold,
  type HelperPromptRecord,
  type UpdateHold,
} from "../lib/helperUpdatePrompt";
import { launchInFlight } from "../lib/launchInFlight";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { trStatic } from "../lib/trStatic";
import { consoleBusy } from "../lib/usePoll";
import { bringUpStatusFor, useBringUpStore } from "./bringUp";
import { useConnectionStore } from "./connection";
import { consoleSettingFor } from "./consoleSettings";
import { helperSendFor, useHelperSendStore } from "./helperSend";
import { sendHelperTo } from "./helperSendRuntime";
import { isHelperStopped } from "./helperStopped";
import type { Translator } from "./lang";
import { log } from "./logs";
import { pushNotification, useNotificationsStore } from "./notifications";
import { useRemotePlayStore } from "./remotePlay";
import { withConsolePrefix } from "./roster";
import { isTerminal, useTaskStore } from "./tasks";
import { useToastStore } from "./toasts";
import { useUploadQueueStore } from "./uploadQueue";

/** One console's notice on Home. */
export interface HelperPromptView {
  host: string;
  /** The helper's version, and this app's. */
  from: string;
  to: string;
}

interface StoreState {
  /** Consoles whose notice shows, by console key (`hostOf`). */
  byHost: Record<string, HelperPromptView>;
  /** Consoles whose helper answers and is older than this app, the notice dismissed or not. */
  outdated: Record<string, true>;
  /** Why the last press of Update helper did not send, by console key. */
  holds: Record<string, UpdateHold>;
  /** An Update helper press waiting for the component that owns the questions (the toast's
   *  button runs outside React). */
  request: { host: string; n: number } | null;
}

export const useHelperUpdatePromptStore = create<StoreState>(() => ({ byHost: {}, outdated: {}, holds: {}, request: null }));

const STORAGE_KEY = "ps5upload.helper-update-prompt.v1";

/** What each console was told, read from storage once and kept in step with it. */
let records: Record<string, HelperPromptRecord> | null = null;

function loadRecords(): Record<string, HelperPromptRecord> {
  if (records) return records;
  try {
    const parsed: unknown = JSON.parse(safeGetItem(STORAGE_KEY) ?? "{}");
    records =
      parsed && typeof parsed === "object" && !Array.isArray(parsed)
        ? (parsed as Record<string, HelperPromptRecord>)
        : {};
  } catch {
    records = {};
  }
  return records;
}

function saveRecord(key: string, patch: HelperPromptRecord): void {
  const all = loadRecords();
  all[key] = { ...all[key], ...patch };
  safeSetItem(STORAGE_KEY, JSON.stringify(all));
}

const named = (key: string, text: string) => withConsolePrefix(key, text);

/** How long the toast stays: longer than a warning's usual 6 s, it carries the button. */
const TOAST_MS = 15_000;

function publish(key: string, view: HelperPromptView | null): void {
  const cur = useHelperUpdatePromptStore.getState().byHost[key];
  if (view === null) {
    if (!cur && !useHelperUpdatePromptStore.getState().holds[key]) return;
    useHelperUpdatePromptStore.setState((s) => {
      const byHost = { ...s.byHost };
      const holds = { ...s.holds };
      delete byHost[key];
      delete holds[key];
      return { byHost, holds };
    });
    return;
  }
  if (cur && cur.from === view.from && cur.to === view.to && cur.host === view.host) return;
  useHelperUpdatePromptStore.setState((s) => ({ byHost: { ...s.byHost, [key]: view } }));
}

/** Looks at every console the poller knows and says what there is to say. Pure of the console:
 *  nothing is asked or sent. */
export function evaluateHelperPrompts(appVersion: string | null): void {
  const rt = useConnectionStore.getState().runtimeByHost;
  const told = loadRecords();
  const shown = new Set<string>();
  const outdated: Record<string, true> = {};
  for (const [key, r] of Object.entries(rt)) {
    if (key === "_") continue;
    const record = told[key];
    const sending = helperSendFor(useHelperSendStore.getState(), key)?.state === "busy";
    const d = helperPromptDecision({
      appVersion,
      helperVersion: r.payloadVersion,
      status: r.payloadStatus,
      power: r.power,
      pairing: r.session === "needs_pairing",
      stopped: isHelperStopped(key),
      sending,
      autoUpdates: helperAutoUpdateGate.available && consoleSettingFor(key, "auto_update_helper"),
      record,
    });
    // The helper answers and is not older any more: the bell entry about it goes too.
    if (!d.outdated && r.payloadStatus === "up" && r.payloadVersion && appVersion && record?.notificationId) {
      useNotificationsStore.getState().remove(record.notificationId);
      saveRecord(key, { notificationId: undefined });
    }
    if (d.outdated) outdated[key] = true;
    // A send is replacing the helper: the old one stops answering before the new one does, and
    // the notice that started the send keeps showing its steps until it ends.
    if (sending && useHelperUpdatePromptStore.getState().byHost[key]) {
      shown.add(key);
      continue;
    }
    if (!d.show || !appVersion || !r.payloadVersion) continue;
    shown.add(key);
    publish(key, { host: key, from: r.payloadVersion, to: appVersion });
    if (!d.notify) continue;
    const title = named(key, trStatic("helper_update_title", "The PS5 helper is out of date"));
    const notificationId = pushNotification("info", title, {
      body: trStatic(
        "helper_update_body",
        "This PS5 runs helper v{helper}. This app is v{app}. Update the helper so new fixes and features work.",
        { helper: r.payloadVersion, app: appVersion },
      ),
      link: "/connection",
    });
    saveRecord(key, { notified: appVersion, notificationId });
    useToastStore.getState().push({
      tone: "warn",
      message: title,
      duration: TOAST_MS,
      action: {
        label: trStatic("helper_update_action", "Update helper"),
        onClick: () => requestHelperUpdate(key),
      },
    });
    log.info("payload", `helper v${r.payloadVersion} on ${key} is older than this app's v${appVersion}: said so (once for this version)`);
  }
  for (const key of Object.keys(useHelperUpdatePromptStore.getState().byHost)) {
    if (!shown.has(key)) publish(key, null);
  }
  const before = useHelperUpdatePromptStore.getState().outdated;
  if (Object.keys(before).sort().join("|") !== Object.keys(outdated).sort().join("|")) {
    useHelperUpdatePromptStore.setState({ outdated });
  }
}

/** Hides the notice on Home for this console until the next app version. */
export function dismissHelperPrompt(host: string): void {
  const key = hostOf(host) || host;
  const view = useHelperUpdatePromptStore.getState().byHost[key];
  if (view) saveRecord(key, { dismissed: view.to });
  publish(key, null);
}

/** What this app is in the middle of on `host` that a new helper would stop, or null. A helper
 *  send already running is not one: pressing the button then shows that send. */
export function helperUpdateHold(host: string): UpdateHold | null {
  const key = hostOf(host) || host;
  const rp = useRemotePlayStore.getState();
  return updateHold({
    transfer:
      transferScreenBusy(host) || consoleBusy(host) || !!useUploadQueueStore.getState().runningHosts[key],
    task: useTaskStore
      .getState()
      .tasks.some(
        (t) =>
          hostOf(t.consoleId) === key &&
          !isTerminal(t.status) &&
          (t.status === "running" || t.status === "paused" || t.status === "awaiting"),
      ),
    launch: launchInFlight(host),
    stream: !!(rp.stream && rp.target && hostOf(rp.target.host) === key),
    setup: bringUpStatusFor(useBringUpStore.getState(), host).kind === "running",
  });
}

/** The sentence for a hold. */
export function holdText(tr: Translator, hold: UpdateHold): string {
  switch (hold) {
    case "transfer":
      return tr(
        "helper_update_hold_transfer",
        undefined,
        "A transfer or an install is running on this PS5, and a new helper would stop it. Update the helper when it has finished.",
      );
    case "launch":
      return tr(
        "helper_update_hold_launch",
        undefined,
        "A game is starting on this PS5. Update the helper once it is running.",
      );
    case "stream":
      return tr(
        "helper_update_hold_stream",
        undefined,
        "This app is showing this PS5's screen, and a new helper would stop it. Update the helper after you close it.",
      );
    default:
      return tr(
        "helper_update_hold_busy",
        undefined,
        "This app is still setting up this PS5. Update the helper when it has finished.",
      );
  }
}

let requests = 0;

/** Update helper was pressed for `host`: the component that owns the questions takes it. */
export function requestHelperUpdate(host: string): void {
  requests += 1;
  useHelperUpdatePromptStore.setState({ request: { host, n: requests } });
}

/** Takes the pending press, once (the shell can be mounted more than once). */
export function takeHelperUpdateRequest(): string | null {
  const r = useHelperUpdatePromptStore.getState().request;
  if (!r) return null;
  useHelperUpdatePromptStore.setState({ request: null });
  return r.host;
}

export type HelperUpdateOutcome = "held" | "declined" | "ok" | "fail" | "busy";

/** The Update helper button: what Resend helper does, and nothing while this app is in the
 *  middle of something on that console (said why, on the notice and as a toast). `guard` is
 *  useHelperReplaceGuard's (other devices' work, the console's own, a downgrade). */
export async function runHelperUpdate(
  host: string,
  tr: Translator,
  guard: (host: string) => Promise<boolean>,
  send: typeof sendHelperTo = sendHelperTo,
): Promise<HelperUpdateOutcome> {
  const key = hostOf(host) || host;
  const hold = helperUpdateHold(host);
  if (hold) {
    useHelperUpdatePromptStore.setState((s) => ({ holds: { ...s.holds, [key]: hold } }));
    useToastStore.getState().push({ tone: "warn", message: named(key, holdText(tr, hold)) });
    log.info("payload", `update helper on ${key}: not sent, this app is busy there (${hold})`);
    return "held";
  }
  useHelperUpdatePromptStore.setState((s) => {
    if (!s.holds[key]) return s;
    const holds = { ...s.holds };
    delete holds[key];
    return { holds };
  });
  if (!(await guard(host))) return "declined";
  // As the console card's Resend helper: the selected console's version is cleared so the screens
  // do not show the old helper as current while the new one starts.
  const conn = useConnectionStore.getState();
  const active = hostOf(conn.host) === key;
  const replacing = active ? conn.payloadVersion : (conn.runtimeByHost[key]?.payloadVersion ?? null);
  if (active) conn.setStatus({ payloadVersion: null, ps5Kernel: null });
  return send(host, tr, replacing);
}

let watchers = 0;
let stopWatch: (() => void) | null = null;

/** Keeps the notices in step with the status poller while at least one caller holds it. Returns
 *  the release. */
export function startHelperUpdatePrompt(): () => void {
  watchers += 1;
  if (watchers === 1) {
    let cancelled = false;
    let app: string | null = null;
    const run = () => {
      if (!cancelled && app) evaluateHelperPrompts(app);
    };
    let asking = false;
    const ask = () => {
      if (asking) return;
      asking = true;
      void getAppVersion()
        .then((v) => {
          app = v;
          run();
        })
        .catch(() => {})
        .finally(() => {
          asking = false;
        });
    };
    ask();
    const unsubscribe = [
      useConnectionStore.subscribe(() => {
        // The engine was not up yet when the app's version was first asked for (the web UI reads
        // it from the engine).
        if (!app) ask();
        run();
      }),
      useHelperSendStore.subscribe(run),
    ];
    stopWatch = () => {
      cancelled = true;
      for (const u of unsubscribe) u();
    };
  }
  let released = false;
  return () => {
    if (released) return;
    released = true;
    watchers -= 1;
    if (watchers === 0) {
      stopWatch?.();
      stopWatch = null;
    }
  };
}

/** Test seam. */
export function resetHelperUpdatePrompt(): void {
  records = null;
  requests = 0;
  useHelperUpdatePromptStore.setState({ byHost: {}, outdated: {}, holds: {}, request: null });
}
