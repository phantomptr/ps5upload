import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";

/**
 * Which PS5 notifications this computer has read, per console.
 *
 * The payload's notification ring has no read state — its entries carry a
 * sequence number, a time, a level and a message, nothing more — so every
 * entry arrived as unread and nothing could ever mark one read. The console
 * has no concept to sync with, so the state lives here.
 *
 * Kept as a watermark plus exceptions, so it stays tiny however long the
 * console runs: everything at or below `upTo` is read unless listed in
 * `unread`; above it, only what is listed in `read`. Sequence numbers never
 * rewind (the payload keeps counting across Clear), so a watermark stays
 * valid.
 */
export interface HostReadState {
  upTo: number;
  read: number[];
  unread: number[];
}

const STORAGE_KEY = "ps5upload.ps5notif_read.v1";
const EMPTY: HostReadState = { upTo: 0, read: [], unread: [] };

function isReadState(v: unknown): v is HostReadState {
  if (!v || typeof v !== "object") return false;
  const s = v as Partial<Record<keyof HostReadState, unknown>>;
  const seqs = (a: unknown) => Array.isArray(a) && a.every((n) => typeof n === "number");
  return typeof s.upTo === "number" && seqs(s.read) && seqs(s.unread);
}

export function loadNotifReadState(): Record<string, HostReadState> {
  try {
    const raw = safeGetItem(STORAGE_KEY);
    const parsed = raw ? (JSON.parse(raw) as unknown) : null;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    // A damaged entry reads as nothing read yet, rather than throwing where it is used.
    return Object.fromEntries(
      Object.entries(parsed).filter((entry): entry is [string, HostReadState] => isReadState(entry[1])),
    );
  } catch {
    return {};
  }
}

export function isNotifRead(state: HostReadState | undefined, seq: number): boolean {
  const s = state ?? EMPTY;
  if (seq <= s.upTo) return !s.unread.includes(seq);
  return s.read.includes(seq);
}

/** Mark everything up to `maxSeq` read — the watermark moves and every
 *  exception below it is dropped. */
export function markAllUpTo(state: HostReadState | undefined, maxSeq: number): HostReadState {
  const s = state ?? EMPTY;
  const upTo = Math.max(s.upTo, maxSeq);
  return { upTo, read: s.read.filter((q) => q > upTo), unread: [] };
}

export function setOneRead(
  state: HostReadState | undefined,
  seq: number,
  read: boolean,
): HostReadState {
  const s = state ?? EMPTY;
  const without = (list: number[]) => list.filter((q) => q !== seq);
  if (seq <= s.upTo) {
    return { ...s, unread: read ? without(s.unread) : [...without(s.unread), seq] };
  }
  return { ...s, read: read ? [...without(s.read), seq] : without(s.read) };
}

interface Ps5NotifReadStore {
  byHost: Record<string, HostReadState>;
  markAll: (host: string, maxSeq: number) => void;
  setRead: (host: string, seq: number, read: boolean) => void;
}

export const usePs5NotifRead = create<Ps5NotifReadStore>((set, get) => {
  const save = (byHost: Record<string, HostReadState>) => {
    set({ byHost });
    safeSetItem(STORAGE_KEY, JSON.stringify(byHost));
  };
  return {
    byHost: loadNotifReadState(),
    markAll: (host, maxSeq) => {
      const h = hostOf(host);
      save({ ...get().byHost, [h]: markAllUpTo(get().byHost[h], maxSeq) });
    },
    setRead: (host, seq, read) => {
      const h = hostOf(host);
      save({ ...get().byHost, [h]: setOneRead(get().byHost[h], seq, read) });
    },
  };
});
