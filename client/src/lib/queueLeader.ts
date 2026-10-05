/* One queue runner per browser profile (final review #7).
 *
 * The self-hosted web UI keeps its upload queue in localStorage, so two open tabs both loaded
 * it, both adopted the same engine job and both drained the queue: a second install of the same
 * package, the same upload started twice. This elects one tab as the runner with a lease in
 * localStorage: the leader rewrites `{id, ts}` every HEARTBEAT_MS; a tab that sees an expired
 * (or released) lease takes over. A tab that is not the leader shows the queue read-only.
 *
 * `navigator.locks` and `crypto.randomUUID` do not exist on a plain-http origin, which is how
 * the self-hosted UI is served, so neither is used here. Without usable localStorage (a private
 * window, blocked site data) nothing can be coordinated and the tab is the leader, as before.
 *
 * The desktop app has one window and one engine: it never elects (see `isTauriEnv`). */

export const LEADER_KEY = "ps5upload.queue.leader";
export const LEASE_TTL_MS = 6000;
export const HEARTBEAT_MS = 2000;
/** After writing a claim, wait this long and read it back: two tabs that claimed in the same
 *  instant both wrote, and the later write is the one that stays. */
export const CLAIM_SETTLE_MS = 150;

/** A tab id without `crypto.randomUUID` (absent on plain-http origins). */
export function makeTabId(): string {
  const rand = () => Math.random().toString(36).slice(2, 10);
  return `${Date.now().toString(36)}-${rand()}${rand()}`;
}

interface Lease {
  id: string;
  ts: number;
}

function parseLease(raw: string | null): Lease | null {
  if (!raw) return null;
  try {
    const v = JSON.parse(raw) as Partial<Lease>;
    if (typeof v.id === "string" && typeof v.ts === "number") return { id: v.id, ts: v.ts };
  } catch {
    /* a corrupt lease is no lease */
  }
  return null;
}

export interface QueueLeaderOptions {
  /** localStorage, or null/undefined when the browser has none. */
  storage?: Pick<Storage, "getItem" | "setItem" | "removeItem"> | null;
  now?: () => number;
  id?: string;
  sleep?: (ms: number) => Promise<void>;
}

export interface QueueLeader {
  readonly id: string;
  /** Whether this tab holds the lease right now (as of the last claim or heartbeat). */
  isLeader(): boolean;
  /** Claims the lease when it is free or expired (or already ours). Resolves to isLeader(). */
  claim(): Promise<boolean>;
  /** Starts the heartbeat and the cross-tab listener; `onChange` fires on every transition. */
  start(onChange: (leader: boolean) => void): void;
  /** Gives the lease up (a closing tab) and stops the heartbeat. */
  release(): void;
}

function defaultStorage(): QueueLeaderOptions["storage"] {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

export function createQueueLeader(opts: QueueLeaderOptions = {}): QueueLeader {
  // Looked up on every use, so a storage that appears or goes away later is honoured.
  const getStorage = () => ("storage" in opts ? opts.storage : defaultStorage());
  const now = opts.now ?? (() => Date.now());
  const sleep = opts.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const id = opts.id ?? makeTabId();
  let leader = false;
  let onChange: ((leader: boolean) => void) | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;
  let claiming: Promise<boolean> | null = null;

  const read = (): Lease | null => {
    try {
      return parseLease(getStorage()?.getItem(LEADER_KEY) ?? null);
    } catch {
      return null;
    }
  };
  const write = (): boolean => {
    try {
      getStorage()?.setItem(LEADER_KEY, JSON.stringify({ id, ts: now() } satisfies Lease));
      return true;
    } catch {
      return false;
    }
  };
  const set = (v: boolean) => {
    if (leader === v) return;
    leader = v;
    onChange?.(v);
  };

  const claimOnce = async (): Promise<boolean> => {
    if (!getStorage()) {
      set(true);
      return true;
    }
    const cur = read();
    if (cur && cur.id !== id && now() - cur.ts < LEASE_TTL_MS) {
      set(false);
      return false;
    }
    if (cur?.id === id && leader) {
      // Already ours: a heartbeat, no settle needed.
      if (!write()) {
        set(true); // storage went away: nothing to coordinate with
        return true;
      }
      return true;
    }
    if (!write()) {
      set(true);
      return true;
    }
    await sleep(CLAIM_SETTLE_MS);
    const after = read();
    set(after?.id === id);
    return leader;
  };

  const claim = (): Promise<boolean> => {
    // One claim at a time: a heartbeat landing during a settle must not start a second one.
    if (!claiming) {
      claiming = claimOnce().finally(() => {
        claiming = null;
      });
    }
    return claiming;
  };

  const onStorage = (e: StorageEvent) => {
    if (e.key !== LEADER_KEY) return;
    // The lease was released (a tab closed) or someone else wrote it: re-evaluate now rather
    // than at the next heartbeat.
    void claim();
  };

  return {
    id,
    isLeader: () => leader,
    claim,
    start(cb) {
      onChange = cb;
      if (timer) return;
      timer = setInterval(() => void claim(), HEARTBEAT_MS);
      (timer as unknown as { unref?: () => void }).unref?.();
      if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
        window.addEventListener("storage", onStorage);
        window.addEventListener("pagehide", relinquish);
      }
    },
    release,
  };

  function release() {
    if (timer) {
      clearInterval(timer);
      timer = null;
    }
    if (typeof window !== "undefined" && typeof window.removeEventListener === "function") {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("pagehide", relinquish);
    }
    relinquish();
  }

  /** The tab is going away (or into the back/forward cache): free the lease for another tab.
   *  The heartbeat keeps running, so a restored page claims it again. */
  function relinquish() {
    try {
      if (read()?.id === id) getStorage()?.removeItem(LEADER_KEY);
    } catch {
      /* best effort */
    }
    leader = false;
  }
}
