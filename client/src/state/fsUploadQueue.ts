/** Additional Files picks wait behind the console's current copy. Requests keep their host,
 * destination and conflict decisions across navigation/restart; failures pause the queue. */
import { create } from "zustand";
import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { FS_UPLOAD_BUSY, fsUploadForHost, runFsUpload, useFsUploadStore, type FsUploadDeps, type FsUploadRequest } from "./fsUpload";
import { nameKey, type FsExistingEntry } from "./fsUploadPlan";

export interface PendingFsUpload {
  id: string;
  request: FsUploadRequest;
  /** Destinations already present (including earlier queued writes) when conflicts were settled. */
  allowedExisting: string[];
}
interface Slot { items: PendingFsUpload[]; paused: boolean; running: boolean; error: string | null }
const IDLE: Slot = { items: [], paused: false, running: false, error: null };
const KEY = "ps5upload.fsUploadQueue.v1";
function load(): Record<string, Slot> {
  try {
    const saved = JSON.parse(safeGetItem(KEY) ?? "{}") as Record<string, Slot>;
    return Object.fromEntries(Object.entries(saved).filter(([, s]) => s && Array.isArray(s.items)).map(([host, s]) => [host, {
      ...s,
      // The transfer store restores an in-flight pick as a resumable stopped run.
      // Leave its progress/resume there instead of sending the original pick twice.
      items: s.items.filter((item) => item.id !== fsUploadForHost(useFsUploadStore.getState(), host).stopped?.queueId),
      running: false, paused: true,
    }]));
  } catch { return {}; }
}
export const useFsUploadQueue = create<{ byHost: Record<string, Slot> }>(() => ({ byHost: load() }));
export const fsUploadQueueFor = (host: string) => useFsUploadQueue.getState().byHost[hostOf(host)] ?? IDLE;
function patch(host: string, p: Partial<Slot>) {
  const key = hostOf(host);
  useFsUploadQueue.setState((s) => ({ byHost: { ...s.byHost, [key]: { ...(s.byHost[key] ?? IDLE), ...p } } }));
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
  if (slot.running && slot.items[0]?.id === id) return;
  patch(host, { items: slot.items.filter((i) => i.id !== id) });
}
export function pauseFsUploadQueue(host: string) { patch(host, { paused: true }); }
export function resumeFsUploadQueue(host: string, deps: FsUploadDeps) {
  runners.set(hostOf(host), deps);
  patch(host, { paused: false, error: null });
  void drainFsUploadQueue(host, deps);
}
export async function drainFsUploadQueue(host: string, deps: FsUploadDeps): Promise<void> {
  const slot = fsUploadQueueFor(host);
  const active = fsUploadForHost(useFsUploadStore.getState(), host);
  if (slot.running || slot.paused || active.active || active.stopped || !slot.items.length) return;
  patch(host, { running: true });
  try {
    while (!fsUploadQueueFor(host).paused) {
      const item = fsUploadQueueFor(host).items[0];
      if (!item) break;
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
        if (!result.ok) { patch(host, { paused: true }); break; }
      } catch (e) {
        patch(host, { paused: true, error: String(e) });
        break;
      }
    }
  } finally { patch(host, { running: false }); }
}
// A copy started outside this queue (Replace / Resume) finishes: pending picks may now run.
useFsUploadStore.subscribe(() => {
  for (const [host, deps] of runners) void drainFsUploadQueue(host, deps);
});
