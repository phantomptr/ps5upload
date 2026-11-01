/** Additional Files picks wait behind the console's current copy. Requests keep their host,
 * destination and conflict decisions across navigation/restart; failures pause the queue.
 *
 * Start rule (#536): this queue never runs a new pick ahead of older ones. It is strictly
 * first-in-first-out, so a drop either starts the whole line from the top (nothing paused)
 * or joins the line behind a pause and runs, in order, on Resume. It does not follow the
 * console queue's "Start the queue when you add something" setting on purpose: a drop into
 * a Files folder is a file-manager copy the person is watching, not a queued send, and it
 * has no "Add to queue" for later.
 *
 * Pause / Resume / Send now: Pause stops the running copy the way Stop does (the console keeps
 * what landed) and turns what is left of it into a paused pick at the front of the line; the
 * line then goes on without it. Resume puts it back in line where it is. Send now runs a
 * waiting pick next, pausing the copy going now, and resumes that copy by itself once every
 * pick sent now has finished. A pick the person paused is never resumed for them. */
import { create } from "zustand";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import {
  cancelFsUpload,
  dismissFsUploadStopped,
  FS_UPLOAD_BUSY,
  fsUploadForHost,
  runFsUpload,
  useFsUploadStore,
  type FsUploadDeps,
  type FsUploadRequest,
  type FsUploadStopped,
} from "./fsUpload";
import { nameKey, type FsExistingEntry } from "./fsUploadPlan";

export interface PendingFsUpload {
  id: string;
  request: FsUploadRequest;
  /** Destinations already present (including earlier queued writes) when conflicts were settled. */
  allowedExisting: string[];
  /** Waits and is skipped until resumed. `user`: the person paused it. `sendNow`: a Send now
   *  paused it, and it resumes by itself once the picks sent now have finished. */
  paused?: "user" | "sendNow" | null;
  /** Sent now: runs ahead of the line (and even while the line is held); clicks keep order. */
  sendNowAt?: number | null;
  /** Why a pick sent now stopped, shown on its paused row. */
  error?: string | null;
}
interface Slot {
  items: PendingFsUpload[]; paused: boolean; running: boolean; error: string | null;
  /** The pick the queue is running now (a pick sent now need not be the first). */
  currentId?: string | null;
}
const IDLE: Slot = { items: [], paused: false, running: false, error: null, currentId: null };
const KEY = "ps5upload.fsUploadQueue.v1";
/** "Paused" holds back picks that are WAITING. With nothing waiting there is nothing to hold:
 *  a pause (a failure, a Cancel, a restart) never outlives the picks it held, so the next drop
 *  into an empty queue starts at once instead of sitting behind a Resume nobody asked for. */
function settle(s: Slot): Slot {
  return s.items.length === 0 ? { ...s, paused: false, error: null } : s;
}
function load(): Record<string, Slot> {
  try {
    const saved = JSON.parse(safeGetItem(KEY) ?? "{}") as Record<string, Slot>;
    return Object.fromEntries(Object.entries(saved).filter(([, s]) => s && Array.isArray(s.items)).map(([host, s]) => {
      // The transfer store restores an in-flight pick as a resumable stopped run.
      // Leave its progress/resume there instead of sending the original pick twice.
      const items = s.items.filter((item) => item.id !== fsUploadForHost(useFsUploadStore.getState(), host).stopped?.queueId);
      // No pick sent now is left (the one that was running came back as a stopped run): the
      // pick it paused waits in line again rather than for a Send now that will not finish.
      const sending = items.some((i) => i.sendNowAt && !i.paused);
      return [host, settle({
        ...s,
        items: sending ? items : items.map((i) => (i.paused === "sendNow" ? { ...i, paused: null } : i)),
        running: false, paused: true, currentId: null,
      })];
    }));
  } catch { return {}; }
}
export const useFsUploadQueue = create<{ byHost: Record<string, Slot> }>(() => ({ byHost: load() }));
export const fsUploadQueueFor = (host: string) => useFsUploadQueue.getState().byHost[hostOf(host)] ?? IDLE;
function patch(host: string, p: Partial<Slot>) {
  const key = hostOf(host);
  useFsUploadQueue.setState((s) => ({ byHost: { ...s.byHost, [key]: settle({ ...(s.byHost[key] ?? IDLE), ...p }) } }));
  safeSetItem(KEY, JSON.stringify(useFsUploadQueue.getState().byHost));
}
function names(req: FsUploadRequest): string[] {
  return req.steps?.flatMap((s) => s.kind === "list" ? s.files.map((f) => f.name) : [s.name]) ?? [];
}
/** Include writes that have not reached the disk yet in the conflict dialog's taken names. */
export function fsUploadReservations(host: string, destDir: string): FsExistingEntry[] {
  return fsUploadQueueFor(host).items.filter((i) => i.request.destDir === destDir)
    .flatMap((i) => names(i.request).map((name) => ({ name, kind: "file", size: 0 })));
}
const runners = new Map<string, FsUploadDeps>();
let seq = 0;
export function enqueueFsUpload(req: FsUploadRequest, deps: FsUploadDeps, existing: FsExistingEntry[]): string {
  const id = `fsq-${Date.now()}-${++seq}`;
  const item = { id, request: JSON.parse(JSON.stringify({ ...req, queueId: id })) as FsUploadRequest, allowedExisting: existing.map((e) => nameKey(e.name)) };
  patch(req.host, { items: [...fsUploadQueueFor(req.host).items, item] });
  runners.set(hostOf(req.host), deps);
  void drainFsUploadQueue(req.host, deps);
  return id;
}
export function removeFsUploadQueued(host: string, id: string) {
  const slot = fsUploadQueueFor(host);
  if (slot.running && slot.currentId === id) return;
  patch(host, { items: slot.items.filter((i) => i.id !== id) });
  settleSendNow(host);
}

/** Consoles whose running copy is being paused, and why: when the copy ends, what is left of
 *  it becomes a paused pick (see `parkStopped`). */
const pausing = new Map<string, "user" | "sendNow">();
/** Consoles whose last run ended in a pause: the drain loop goes on instead of holding. */
const parked = new Set<string>();

/** The next pick to run: the earliest sent now, else (when the line is not held) the first one
 *  not paused. Strictly in line otherwise (#536). */
function pick(slot: Slot, held: boolean): PendingFsUpload | undefined {
  let sent: PendingFsUpload | undefined;
  for (const i of slot.items) {
    if (i.paused || !i.sendNowAt) continue;
    if (!sent || i.sendNowAt < (sent.sendNowAt ?? 0)) sent = i;
  }
  if (sent || held) return sent;
  return slot.items.find((i) => !i.paused);
}

/** Turns the console's stopped run into a paused pick at the front of the line (it was ahead
 *  of every waiting pick) and clears the stopped card. Resuming it sends the same steps under
 *  the same job ids, so the console's journal skips what it already has. */
function parkStopped(
  host: string,
  why: "user" | "sendNow",
  st: FsUploadStopped,
  error?: string,
  inQueue = true,
) {
  const id = `fsq-${Date.now()}-${++seq}`;
  const request: FsUploadRequest = {
    host,
    queueId: id,
    addr: st.addr,
    destDir: st.destDir,
    srcPaths: st.srcPaths,
    steps: st.steps,
    replaceRemoteName: st.replaceRemoteName,
    doneBefore: st.stepsDone ?? st.doneCount,
    countTotal: st.count,
  };
  const item: PendingFsUpload = {
    id,
    request: JSON.parse(JSON.stringify(request)) as FsUploadRequest,
    // Its own partial files are on the console now: they are not a conflict.
    allowedExisting: names(request).map(nameKey),
    paused: why,
    error: error ?? null,
  };
  patch(host, { items: [item, ...fsUploadQueueFor(host).items] });
  // Only the queue's own drain loop reads (and clears) the mark. Set it after the dismiss
  // below: that store change can start the drain loop for the next pick at once.
  dismissFsUploadStopped(host);
  if (inQueue) parked.add(hostOf(host));
}

/** Once no pick sent now is waiting or running on `host`, the picks a Send now paused go back
 *  in line. */
function settleSendNow(host: string) {
  const slot = fsUploadQueueFor(host);
  if (slot.items.some((i) => i.sendNowAt && !i.paused)) return;
  if (!slot.items.some((i) => i.paused === "sendNow")) return;
  patch(host, { items: slot.items.map((i) => (i.paused === "sendNow" ? { ...i, paused: null } : i)) });
}

/** Pause the console's running copy: it stops the way Stop does (what landed stays), and
 *  what is left of it waits as a paused pick while the line goes on. */
export function pauseFsUpload(host: string, deps: FsUploadDeps, why: "user" | "sendNow" = "user") {
  if (!fsUploadForHost(useFsUploadStore.getState(), host).active) return;
  pausing.set(hostOf(host), why);
  cancelFsUpload(host, deps);
}

/** Put a paused pick back in line where it is; with nothing running it starts now. */
export function resumeFsUploadItem(host: string, id: string, deps: FsUploadDeps) {
  const slot = fsUploadQueueFor(host);
  if (!slot.items.some((i) => i.id === id && i.paused)) return;
  runners.set(hostOf(host), deps);
  patch(host, {
    items: slot.items.map((i) => (i.id === id ? { ...i, paused: null, error: null } : i)),
    paused: false,
    error: null,
  });
  void drainFsUploadQueue(host, deps);
}

/** Run a waiting pick next. The copy going now is paused, unless it was sent now too (then
 *  this one goes after it), and it carries on by itself once the picks sent now are done. */
export function sendFsUploadNow(host: string, id: string, deps: FsUploadDeps) {
  const slot = fsUploadQueueFor(host);
  if (!slot.items.some((i) => i.id === id && !i.paused)) return;
  const key = hostOf(host);
  runners.set(key, deps);
  const at = Math.max(Date.now(), ...slot.items.map((i) => (i.sendNowAt ?? 0) + 1));
  patch(host, { items: slot.items.map((i) => (i.id === id ? { ...i, sendNowAt: at } : i)) });
  const cur = slot.running ? slot.currentId : null;
  const runningSent = slot.running && cur ? !!slot.items.find((i) => i.id === cur)?.sendNowAt : false;
  if (fsUploadForHost(useFsUploadStore.getState(), host).active && !runningSent && cur !== id) {
    pauseFsUpload(host, deps, "sendNow");
    return;
  }
  void drainFsUploadQueue(host, deps);
}
export function pauseFsUploadQueue(host: string) { patch(host, { paused: true }); }
export function resumeFsUploadQueue(host: string, deps: FsUploadDeps) {
  runners.set(hostOf(host), deps);
  patch(host, { paused: false, error: null });
  void drainFsUploadQueue(host, deps);
}
export async function drainFsUploadQueue(host: string, deps: FsUploadDeps): Promise<void> {
  const key = hostOf(host);
  // A paused queue, or a stopped run waiting for Resume, holds the line; a pick sent now
  // still goes.
  const held = () => fsUploadQueueFor(host).paused || !!fsUploadForHost(useFsUploadStore.getState(), host).stopped;
  const slot = fsUploadQueueFor(host);
  const active = fsUploadForHost(useFsUploadStore.getState(), host);
  if (slot.running || active.active || !pick(slot, held())) return;
  patch(host, { running: true });
  try {
    for (;;) {
      const item = pick(fsUploadQueueFor(host), held());
      if (!item) break;
      patch(host, { currentId: item.id });
      try {
        // A queued pick can wait minutes. Never overwrite a newly-created destination the
        // user did not approve replacing. Leave the item queued so it can be removed/replanned.
        if (deps.listDir) {
          const present = await deps.listDir(item.request.addr, item.request.destDir);
          const allowed = new Set(item.allowedExisting);
          const taken = new Set(present.map((e) => nameKey(e.name)));
          const conflict = names(item.request).find((n) => taken.has(nameKey(n)) && !allowed.has(nameKey(n)));
          if (conflict) throw new Error(`Destination changed while queued: ${conflict}. Remove this queued pick and add it again to choose Replace, Keep both or Skip.`);
        }
        const result = await runFsUpload(item.request, deps);
        if (result.error === FS_UPLOAD_BUSY) break;
        patch(host, { items: fsUploadQueueFor(host).items.filter((i) => i.id !== item.id) });
        // Paused (by the person or a Send now): what is left waits as a paused pick; go on.
        if (parked.delete(key)) { settleSendNow(host); continue; }
        if (!result.ok) {
          const st = fsUploadForHost(useFsUploadStore.getState(), host).stopped;
          // A pick sent now that failed does not hold the line: it waits, paused with its
          // error, and the copy it paused carries on.
          if (item.sendNowAt && st && st.why === "failed") {
            parkStopped(host, "user", st, st.error);
            parked.delete(key);
            settleSendNow(host);
            continue;
          }
          patch(host, { paused: true });
          settleSendNow(host);
          break;
        }
        settleSendNow(host);
      } catch (e) {
        patch(host, { paused: true, error: String(e) });
        settleSendNow(host);
        break;
      }
    }
  } finally { patch(host, { running: false, currentId: null }); }
}
// A copy started outside this queue (Replace / Resume) finishes: pending picks may now run.
// A copy being paused has ended: what is left of it becomes a paused pick.
useFsUploadStore.subscribe(() => {
  for (const [key, why] of pausing) {
    const st = fsUploadForHost(useFsUploadStore.getState(), key);
    if (st.active) continue;
    pausing.delete(key);
    // A copy outside the queue (Replace / Resume) has no drain loop to read the pause mark.
    // Read before parking: parking can start the loop for the next pick, and that pick's
    // ending must not be taken for this pause.
    const inQueue = fsUploadQueueFor(key).running;
    // It finished before the stop landed: nothing to pause.
    if (st.stopped) parkStopped(key, why, st.stopped, undefined, inQueue);
  }
  for (const [host, deps] of runners) void drainFsUploadQueue(host, deps);
});
