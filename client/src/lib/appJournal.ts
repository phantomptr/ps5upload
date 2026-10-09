/**
 * The app's event journal (bug-report spec §1.4): what the app itself saw go wrong, kept 7 days
 * so a report filed later still has it. Desktop/Android store it in a file through Tauri
 * (`app_events_append` / `app_events_read`); the web UI in IndexedDB, which is per browser.
 * Repeats within a minute are folded here; writes are batched each second and never throw.
 */
import { invoke } from "@tauri-apps/api/core";
import { isTauriEnv } from "./tauriEnv";
import { COLLAPSE_MS, collapseInto, type EventRecord } from "./eventRecord";

export const APP_JOURNAL_IDB = {
  db: "ps5upload-events",
  store: "events",
  maxRecords: 20_000,
  maxAgeMs: 7 * 86_400_000,
};
const MSG_MAX = 1024;

export interface JournalBackend {
  append(lines: string[]): Promise<void>;
  read(since: number, until: number): Promise<string[]>;
  now(): number;
}

export type NewAppEvent = Omit<EventRecord, "ts" | "src"> & { ts?: number };

export function createJournal(b: JournalBackend) {
  let pending: EventRecord | null = null;
  let queue: string[] = [];
  let dropped = 0;
  const push = (r: EventRecord) => queue.push(JSON.stringify(r));
  const write = async () => {
    if (queue.length === 0) return;
    const lines = queue;
    queue = [];
    try {
      await b.append(lines);
    } catch {
      dropped += lines.length;
    }
  };
  const flush = async () => {
    if (pending) {
      push(pending);
      pending = null;
    }
    await write();
  };
  return {
    /** Records an app event; returns its time (what a "Report this" link points at). */
    record(e: NewAppEvent): number {
      const r: EventRecord = { ...e, ts: e.ts ?? b.now(), src: "app", msg: e.msg.slice(0, MSG_MAX) };
      const { merged, flushed } = collapseInto(pending, r);
      pending = merged;
      if (flushed) push(flushed);
      return r.ts;
    },
    async tick() {
      if (pending && b.now() - (pending.last_ts ?? pending.ts) > COLLAPSE_MS) {
        push(pending);
        pending = null;
      }
      await write();
    },
    flush,
    async read(since: number, until = Number.MAX_SAFE_INTEGER): Promise<EventRecord[]> {
      await flush();
      const lines = await b.read(since, until).catch(() => [] as string[]);
      return lines.flatMap((l) => {
        try {
          return [JSON.parse(l) as EventRecord];
        } catch {
          return [];
        }
      });
    },
    dropped: () => dropped,
  };
}

function idbBackend(): JournalBackend {
  const open = () =>
    new Promise<IDBDatabase>((res, rej) => {
      const req = indexedDB.open(APP_JOURNAL_IDB.db, 1);
      req.onupgradeneeded = () =>
        req.result.createObjectStore(APP_JOURNAL_IDB.store, { autoIncrement: true }).createIndex("ts", "ts");
      req.onsuccess = () => res(req.result);
      req.onerror = () => rej(req.error);
    });
  const done = (tx: IDBTransaction) =>
    new Promise<void>((res, rej) => {
      tx.oncomplete = () => res();
      tx.onerror = () => rej(tx.error);
      tx.onabort = () => rej(tx.error);
    });
  return {
    now: () => Date.now(),
    async append(lines) {
      const db = await open();
      const tx = db.transaction(APP_JOURNAL_IDB.store, "readwrite");
      const st = tx.objectStore(APP_JOURNAL_IDB.store);
      for (const l of lines) st.add({ ts: (JSON.parse(l) as EventRecord).ts, line: l });
      // Prune: older than 7 days, then the oldest beyond the record cap.
      st.index("ts").openCursor(IDBKeyRange.upperBound(Date.now() - APP_JOURNAL_IDB.maxAgeMs)).onsuccess =
        function () {
          const c = this.result;
          if (c) {
            c.delete();
            c.continue();
          }
        };
      const countReq = st.count();
      countReq.onsuccess = () => {
        let extra = countReq.result - APP_JOURNAL_IDB.maxRecords;
        if (extra <= 0) return;
        st.openCursor().onsuccess = function () {
          const c = this.result;
          if (c && extra-- > 0) {
            c.delete();
            c.continue();
          }
        };
      };
      await done(tx);
      db.close();
    },
    async read(since, until) {
      const db = await open();
      const tx = db.transaction(APP_JOURNAL_IDB.store, "readonly");
      const out: string[] = [];
      // A folded record can start before `since`: scan from an hour earlier.
      tx.objectStore(APP_JOURNAL_IDB.store).index("ts").openCursor(IDBKeyRange.bound(Math.max(0, since - 3_600_000), until)).onsuccess =
        function () {
          const c = this.result;
          if (c) {
            out.push((c.value as { line: string }).line);
            c.continue();
          }
        };
      await done(tx);
      db.close();
      return out.filter((l) => {
        const r = JSON.parse(l) as EventRecord;
        return (r.last_ts ?? r.ts) >= since;
      });
    },
  };
}

function tauriBackend(): JournalBackend {
  return {
    now: () => Date.now(),
    append: (lines) => invoke("app_events_append", { lines }),
    read: (since, until) => invoke<string[]>("app_events_read", { sinceMs: since, untilMs: until }),
  };
}

const noBackend: JournalBackend = { now: () => Date.now(), append: async () => {}, read: async () => [] };

const journal = createJournal(
  isTauriEnv() ? tauriBackend() : typeof indexedDB !== "undefined" ? idbBackend() : noBackend,
);
if (typeof window !== "undefined") globalThis.setInterval(() => void journal.tick(), 1000);

/** Records an app event; returns its time. Never throws. */
export const recordAppEvent = (e: NewAppEvent): number => journal.record(e);
export const readAppEvents = (since: number, until?: number) => journal.read(since, until);
export const flushAppJournal = () => journal.flush();
export const appJournalDropped = () => journal.dropped();
