// The devices that use each console (8.0.0): who is connected, what each is doing, the locks
// that keep two devices off the same install or folder, and the events they see. Polled per
// console while the app is open; this window's own work is published as its presence.
//
// An older helper answers "not supported": everything here then stays empty and the app works
// as it did before 8.0.0.

import { create } from "zustand";

import {
  fetchMeshMe,
  fetchMeshState,
  meshLock,
  postMeshActivity,
  type MeshActivityItem,
  type MeshJoin,
  type MeshDevice,
  type MeshEvent,
  type MeshMe,
  type MeshState,
} from "../api/mesh";
import { hostOf } from "../lib/addr";
import { isTerminal, type Task } from "./tasks";
import { shortLabel } from "./activitySummary";

export interface ConsoleMesh {
  /** null until the first answer; false for a helper (or engine) from before 8.0.0. */
  supported: boolean | null;
  /** The engine itself predates 8.0.0 (not the helper). */
  engineOld?: boolean;
  state: MeshState | null;
  error: string | null;
  /** When the last answer came (ms). */
  at: number;
}

interface MeshStore {
  byHost: Record<string, ConsoleMesh>;
  me: MeshMe | null;
  put: (host: string, patch: Partial<ConsoleMesh>) => void;
  setMe: (me: MeshMe | null) => void;
}

const EMPTY: ConsoleMesh = { supported: null, state: null, error: null, at: 0 };

export const useMeshStore = create<MeshStore>((set) => ({
  byHost: {},
  me: null,
  put: (host, patch) =>
    set((s) => {
      const key = hostOf(host);
      return { byHost: { ...s.byHost, [key]: { ...(s.byHost[key] ?? EMPTY), ...patch } } };
    }),
  setMe: (me) => set({ me }),
}));

export function meshFor(s: { byHost: Record<string, ConsoleMesh> }, host: string): ConsoleMesh {
  return s.byHost[hostOf(host)] ?? EMPTY;
}

// ---- this window ----

let clientIdMemo: string | null = null;

/** One id per app window (each browser tab of a Docker engine is its own window). */
export function meshClientId(): string {
  if (clientIdMemo) return clientIdMemo;
  const KEY = "ps5upload.mesh.client";
  let id: string | null;
  try {
    id = window.sessionStorage.getItem(KEY);
  } catch {
    id = null;
  }
  if (!id) {
    id = Math.random().toString(36).slice(2, 12) + Date.now().toString(36);
    try {
      window.sessionStorage.setItem(KEY, id);
    } catch {
      // A private window: the id lives as long as this page.
    }
  }
  clientIdMemo = id;
  return id;
}

// ---- polling ----

type EventListener = (host: string, e: MeshEvent, state: MeshState) => void;
const eventListeners = new Set<EventListener>();

/** Called for every event another device caused, once, as it arrives. */
export function onMeshEvent(fn: EventListener): () => void {
  eventListeners.add(fn);
  return () => eventListeners.delete(fn);
}

const cursors = new Map<string, { rev: number; ev: number; primed: boolean }>();

/** One poll of `host`. Merges the answer into the store and hands new events to the listeners
 *  (not the ones this device caused, and not the backlog the first poll brings). */
export function pollMesh(host: string): Promise<void> {
  const key = hostOf(host);
  if (!key) return Promise.resolve();
  // One read per console at a time: a second caller (the tick, the window coming back, a Refresh
  // button) shares it, so the cursor never goes back and no event is announced twice.
  const running = polling.get(key);
  if (running) return running;
  const p = pollOnce(key).finally(() => polling.delete(key));
  polling.set(key, p);
  return p;
}

const polling = new Map<string, Promise<void>>();

async function pollOnce(key: string): Promise<void> {
  const cur = cursors.get(key) ?? { rev: 0, ev: 0, primed: false };
  const put = useMeshStore.getState().put;
  try {
    const a = await fetchMeshState(key, cur.rev, cur.ev, meshClientId());
    if (!a.supported) {
      put(key, { supported: false, engineOld: !!a.engineOld, state: null, error: null, at: Date.now() });
      return;
    }
    if (a.same) {
      put(key, { supported: true, error: null, at: Date.now() });
      return;
    }
    const state = a as MeshState;
    // The helper restarted (its event numbers start again): forget what was seen of the old one.
    const restarted = cur.primed && state.ev < cur.ev;
    if (restarted) cur.ev = 0;
    const prev = restarted ? null : meshFor(useMeshStore.getState(), key).state;
    // Events arrive only after the cursor; keep the recent ones for the Devices screen.
    const events = [...(prev?.events ?? []), ...state.events].filter(
      (e, i, all) => all.findIndex((x) => x.seq === e.seq) === i,
    );
    const merged: MeshState = { ...state, events: events.slice(-32) };
    put(key, { supported: true, state: merged, error: null, at: Date.now() });
    if (cur.primed) {
      for (const e of state.events) {
        if (e.seq <= cur.ev || e.by === state.you) continue;
        for (const fn of eventListeners) {
          try {
            fn(key, e, merged);
          } catch {
            // A listener's failure is its own.
          }
        }
      }
    }
    cursors.set(key, { rev: state.rev, ev: state.ev, primed: true });
  } catch (e) {
    put(key, { error: e instanceof Error ? e.message : String(e), at: Date.now() });
  }
}

/** Forget what was seen of a console, so the next poll reads everything again. */
export function resetMeshCursor(host: string) {
  cursors.delete(hostOf(host));
}

export async function loadMeshMe(): Promise<MeshMe | null> {
  try {
    const me = await fetchMeshMe();
    useMeshStore.getState().setMe(me);
    return me;
  } catch {
    return null;
  }
}

// ---- presence ----

/** This window's work on `host`, as the other devices will see it. */
export function presenceItems(tasks: readonly Task[], host: string): MeshActivityItem[] {
  const key = hostOf(host);
  return tasks
    // A console page's task is every device's to see on its own (state/consoleTasks), not this one's work.
    .filter((t) => !isTerminal(t.status) && t.status !== "queued" && t.kind !== "console-task" && hostOf(t.consoleId) === key)
    .slice(0, 8)
    .map((t) => {
      const total = t.progress?.total ?? 0;
      const done = t.progress?.current ?? 0;
      const pct = total > 0 ? Math.max(0, Math.min(100, Math.round((done / total) * 100))) : null;
      return {
        kind: shortLabel(t.kind),
        title: t.label.slice(0, 80),
        pct,
        ...(t.stage ? { stage: t.stage.slice(0, 40) } : {}),
      };
    });
}

const lastPosted = new Map<string, { json: string; at: number }>();

/** Sends this window's presence when it changed, or every `refreshMs` so the engine knows the
 *  window is still there. */
export async function publishPresence(host: string, items: MeshActivityItem[], refreshMs = 8000): Promise<void> {
  const key = hostOf(host);
  if (!key || meshFor(useMeshStore.getState(), key).supported !== true) return;
  const json = JSON.stringify(items);
  const last = lastPosted.get(key);
  if (last && last.json === json && Date.now() - last.at < refreshMs) return;
  lastPosted.set(key, { json, at: Date.now() });
  try {
    await postMeshActivity(key, meshClientId(), items);
  } catch {
    lastPosted.delete(key); // try again next time
  }
}

// ---- reading the picture ----

export function deviceName(d: Pick<MeshDevice, "nick" | "name" | "app">): string {
  if (d.nick) return d.nick;
  // An app from before 8.0.0 only tells the console "ps5upload".
  if (!d.app && (!d.name || d.name === "ps5upload")) return "PS5Upload (older version)";
  return d.name || "A device";
}

/** The other devices online right now, and what each is doing (browsers counts left out). */
export function othersBusy(state: MeshState | null): { device: MeshDevice; items: MeshActivityItem[] }[] {
  if (!state) return [];
  return state.devices
    .filter((d) => d.online && d.id !== state.you)
    .map((d) => ({ device: d, items: d.activity.filter((a) => a.kind !== "browsers") }))
    .filter((x) => x.items.length > 0);
}

/** The presence item a window shows while it plays Remote Play on that console. */
export const RP_ACTIVITY = "Remote Play";

/** The other devices playing Remote Play on this console right now (a PS5 takes one at a time). */
export function othersRemotePlaying(state: MeshState | null): MeshDevice[] {
  return othersBusy(state)
    .filter((x) => x.items.some((a) => a.kind === RP_ACTIVITY))
    .map((x) => x.device);
}

/** The other devices playing Remote Play that take a phone as a controller, and where. */
export function remotePlayPads(state: MeshState | null): Array<{ device: MeshDevice; pad: string }> {
  return othersBusy(state).flatMap((x) => {
    const pad = x.items.find((a) => a.kind === RP_ACTIVITY)?.pad;
    return typeof pad === "string" && pad.startsWith("ws://") ? [{ device: x.device, pad }] : [];
  });
}

/** The other devices hosting Play Together (#641), each once; never this app's own engine
 *  (`ownWs`: the socket this app publishes). */
export function nearbyJoins(state: MeshState | null, ownWs: string | null = null): Array<{ device: MeshDevice; join: MeshJoin }> {
  const seen = new Set<string>();
  return othersBusy(state).flatMap((x) => {
    const join = x.items.find((a) => a.kind === RP_ACTIVITY)?.join;
    if (!join || typeof join.ws !== "string" || !/^wss?:\/\//.test(join.ws) || join.ws === ownWs || seen.has(join.ws)) return [];
    seen.add(join.ws);
    return [{ device: x.device, join }];
  });
}

export function othersOnline(state: MeshState | null): MeshDevice[] {
  if (!state) return [];
  return state.devices.filter((d) => d.online && d.id !== state.you);
}

// ---- locks ----

export type LockOutcome =
  | { ok: true; release: () => Promise<void> }
  | { ok: false; byName: string; label: string };

/**
 * Takes `name` on `host` for as long as the work runs (renewed every 30 s). An older helper,
 * or an engine that cannot be reached, gives a lock that is no lock: the work goes ahead as it
 * did before 8.0.0. `force` takes it over from the device holding it.
 */
export async function takeMeshLock(
  host: string,
  name: string,
  label: string,
  force = false,
): Promise<LockOutcome> {
  const key = hostOf(host);
  const none: LockOutcome = { ok: true, release: async () => {} };
  let a;
  try {
    a = await meshLock(key, "take", name, { ttl: 90, force, label, owner: meshClientId() });
  } catch {
    return none;
  }
  if (!a.supported) return none;
  if (!a.got) return { ok: false, byName: a.byName, label: a.label };
  const timer = setInterval(() => {
    void meshLock(key, "take", name, { ttl: 90, label, owner: meshClientId() }).catch(() => {});
  }, 30_000);
  return {
    ok: true,
    release: async () => {
      clearInterval(timer);
      try {
        await meshLock(key, "release", name, { owner: meshClientId() });
      } catch {
        // It lapses by itself.
      }
    },
  };
}
