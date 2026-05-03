import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

/**
 * Install Package queue. Sequential, like uploadQueue — one BGFT
 * install at a time on the PS5 because Sony's BGFT service serializes
 * concurrent registers internally and we don't want the second one
 * to deadlock against the first.
 *
 * Persisted to localStorage (the user's input items) so a queue
 * survives an app restart. Status fields are NOT persisted — on boot
 * we reset everything to "pending" because we don't know whether a
 * previously-running BGFT task is still alive on the PS5.
 *
 * Runner pattern matches uploadQueue.ts:
 *   - generation-counted via `runId`; stop() bumps it
 *   - the worker loop awaits between async calls and re-checks runId
 *   - per-item progress is polled from the engine every 1 s
 */

export type InstallStatus =
  | "pending"
  | "running"
  | "done"
  | "failed"
  | "cancelled";

export type InstallPhase =
  | "idle"
  | "queued"
  | "download"
  | "install"
  | "done"
  | "error";

export interface InstallQueueItem {
  id: string;
  /** Absolute path of the lead .pkg. For split-pkg sets, the
   *  `<base>.pkg` file. */
  pkgPath: string;
  /** True if `pkgPath` is the lead of a split-pkg set; engine will
   *  auto-detect siblings (`.0`, `.1`, ...). */
  isSplit: boolean;
  /** Display name — basename of pkgPath, or the parsed title when
   *  available. Set at parse-time for nicer rows. */
  displayName: string;
  /** Parsed metadata (set after first parse). content_id, size, etc. */
  contentId: string;
  totalBytes: number;
  /** BGFT package_type (PS4GD/PS4AC/...) — derived from PARAM.SFO
   *  or set explicitly via the override picker. */
  packageType: string;
  /** PS5 mgmt-port addr ("ip:9114"). Captured at add-time so a
   *  later connection-tab change doesn't redirect old queue items. */
  addr: string;
  status: InstallStatus;
  phase: InstallPhase;
  bytesDownloaded: number;
  errCode: number;
  errMessage: string | null;
  /** Set after install/start succeeds — used for status polling. */
  sessionId: string | null;
  taskId: number | null;
  addedAt: number;
  startedAt: number | null;
  finishedAt: number | null;
  /** Warnings from the parse step (e.g. unknown magic). Surfaced as
   *  a yellow caution row before install starts. */
  warnings: string[];
  /** Optional absolute path on the PS5 where the `.pkg` already
   *  lives. When set, the engine builds a `file://` URL instead of
   *  spinning up the HTTP host. The user typically populates this
   *  after first uploading the `.pkg` via the Upload tab; see
   *  the Install Package screen for the picker. */
  localPs5Path: string | null;
}

interface InstallQueueState {
  items: InstallQueueItem[];
  runId: number;
  isRunning: boolean;

  add(item: Omit<InstallQueueItem, "id" | "addedAt" | "status" | "phase" |
    "bytesDownloaded" | "errCode" | "errMessage" | "sessionId" | "taskId" |
    "startedAt" | "finishedAt" | "localPs5Path">): void;
  /** Set the PS5-side path for an existing queue item (the
   *  upload-then-install flow). Persists across reloads. */
  setLocalPs5Path(id: string, path: string | null): void;
  remove(id: string): void;
  clearFinished(): void;
  retry(id: string): void;

  start(): Promise<void>;
  stop(): void;
  cancel(id: string): Promise<void>;

  hydrate(): void;
}

const STORAGE_KEY = "ps5upload.install_queue.v1";

function persistNow(items: InstallQueueItem[]) {
  try {
    const persisted = items.map((it) => ({
      ...it,
      // Don't carry transient runtime state across restarts — we
      // can't trust a previously-"running" status because the BGFT
      // task it referred to may be gone.
      status: it.status === "running" ? "pending" : it.status,
      phase: "idle" as InstallPhase,
      bytesDownloaded: 0,
      sessionId: null,
      taskId: null,
      startedAt: null,
      finishedAt: it.status === "done" ? it.finishedAt : null,
    }));
    localStorage.setItem(STORAGE_KEY, JSON.stringify(persisted));
  } catch {
    // localStorage can throw on quota; the queue still works in
    // memory, the user just loses persistence for this session.
  }
}

// Debounced persist. The runner polls status at 1Hz and would
// otherwise call persist() on every tick — for a multi-hour install
// across a queue of 10 items, that's thousands of redundant
// JSON.stringify + localStorage writes. We coalesce them into a
// single write at most every 500ms.
//
// Terminal-state transitions (markDone / markFailed / markCancelled,
// add / remove / clearFinished, retry) MUST persist immediately so
// a sudden app exit between debounce delay and write doesn't lose
// the user's queue state. Those call sites use `persist()` directly,
// which flushes any pending debounced write first.
let persistTimer: ReturnType<typeof setTimeout> | null = null;
let persistPending: InstallQueueItem[] | null = null;

function persistDebounced(items: InstallQueueItem[]) {
  persistPending = items;
  if (persistTimer) return;
  persistTimer = setTimeout(() => {
    const snap = persistPending;
    persistPending = null;
    persistTimer = null;
    if (snap) persistNow(snap);
  }, 500);
}

/** Flush any pending debounced write + write `items` synchronously.
 *  Used at terminal-state transitions where losing the latest items
 *  to an app crash would orphan a session id in localStorage. */
function persist(items: InstallQueueItem[]) {
  if (persistTimer) {
    clearTimeout(persistTimer);
    persistTimer = null;
    persistPending = null;
  }
  persistNow(items);
}

function load(): InstallQueueItem[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw) as Partial<InstallQueueItem>[];
    // Defensive: drop anything that's missing required fields,
    // and back-fill new optional fields for items persisted by
    // older app versions (so an upgrade doesn't break hydration).
    return parsed
      .filter(
        (it): it is InstallQueueItem =>
          typeof it.id === "string" &&
          typeof it.pkgPath === "string" &&
          typeof it.addr === "string",
      )
      .map((it) => ({
        ...it,
        localPs5Path: it.localPs5Path ?? null,
      }));
  } catch {
    return [];
  }
}

function newId(): string {
  // Random 9-char id; doesn't need to be cryptographically unique,
  // just stable + non-colliding within the queue.
  return Math.random().toString(36).slice(2, 11);
}

export const useInstallQueue = create<InstallQueueState>((set, get) => ({
  items: [],
  runId: 0,
  isRunning: false,

  hydrate() {
    set({ items: load() });
  },

  add(input) {
    const item: InstallQueueItem = {
      ...input,
      id: newId(),
      status: "pending",
      phase: "idle",
      bytesDownloaded: 0,
      errCode: 0,
      errMessage: null,
      sessionId: null,
      taskId: null,
      addedAt: Date.now(),
      startedAt: null,
      finishedAt: null,
      localPs5Path: null,
    };
    const items = [...get().items, item];
    set({ items });
    persist(items);
  },

  remove(id) {
    const items = get().items.filter((it) => it.id !== id);
    set({ items });
    persist(items);
  },

  setLocalPs5Path(id, path) {
    // Normalise: empty string → null so the engine sees a clean
    // optional and falls back to the HTTP-host flow as expected.
    const normalised = path && path.trim() !== "" ? path.trim() : null;
    const items = get().items.map((it) =>
      it.id === id ? { ...it, localPs5Path: normalised } : it,
    );
    set({ items });
    persist(items);
  },

  clearFinished() {
    const items = get().items.filter(
      (it) =>
        it.status !== "done" &&
        it.status !== "failed" &&
        it.status !== "cancelled",
    );
    set({ items });
    persist(items);
  },

  retry(id) {
    const items = get().items.map((it) =>
      it.id === id
        ? {
            ...it,
            status: "pending" as InstallStatus,
            phase: "idle" as InstallPhase,
            bytesDownloaded: 0,
            errCode: 0,
            errMessage: null,
            sessionId: null,
            taskId: null,
            startedAt: null,
            finishedAt: null,
          }
        : it,
    );
    set({ items });
    persist(items);
  },

  async cancel(id) {
    const it = get().items.find((x) => x.id === id);
    if (!it || !it.sessionId) {
      // Pending or no session yet — just drop to cancelled state.
      const items = get().items.map((x) =>
        x.id === id
          ? { ...x, status: "cancelled" as InstallStatus, finishedAt: Date.now() }
          : x,
      );
      set({ items });
      persist(items);
      return;
    }
    try {
      await invoke("pkg_install_cancel", { session: it.sessionId });
    } catch (e) {
      // Continue marking cancelled anyway — the user clicked cancel,
      // they want it gone from their queue regardless of whether
      // we successfully told the engine.
      console.warn("pkg_install_cancel failed:", e);
    }
    const items = get().items.map((x) =>
      x.id === id
        ? {
            ...x,
            status: "cancelled" as InstallStatus,
            finishedAt: Date.now(),
          }
        : x,
    );
    set({ items });
    persist(items);
  },

  stop() {
    // Bump runId — the worker loop sees this on its next await and
    // exits without advancing to the next pending item.
    set({ runId: get().runId + 1, isRunning: false });
  },

  async start() {
    if (get().isRunning) return;
    const myRun = get().runId + 1;
    set({ runId: myRun, isRunning: true });

    const isLive = () => get().runId === myRun;

    while (isLive()) {
      const next = get().items.find((it) => it.status === "pending");
      if (!next) break;

      // Mark running.
      {
        const items = get().items.map((it) =>
          it.id === next.id
            ? { ...it, status: "running" as InstallStatus, startedAt: Date.now() }
            : it,
        );
        set({ items });
        persist(items);
      }

      let startResp: {
        session_id?: string;
        task_id?: number;
        err_code?: number;
        err_message?: string;
        detail?: string;
      };
      try {
        startResp = (await invoke("pkg_install_start", {
          ps5Addr: next.addr,
          path: next.isSplit ? null : next.pkgPath,
          splitRoot: next.isSplit ? next.pkgPath : null,
          packageTypeOverride: next.packageType || null,
          // When set, the engine builds a `file://` URL and skips
          // the HTTP-host setup. Used for the upload-then-install
          // flow: user uploads the .pkg via the Upload tab to a
          // PS5 path, then pastes that path here. Sony's installer
          // reads from local disk — much more reliable than the
          // HTTP-pull on most firmware/network combos.
          localPs5Path: next.localPs5Path || null,
        })) as typeof startResp;
      } catch (e) {
        markFailed(get, set, next.id, 0, `${e}`);
        if (!isLive()) return;
        continue;
      }

      if (!isLive()) return;

      const sessionId = startResp.session_id ?? null;
      const taskId = startResp.task_id ?? null;
      const startErr = startResp.err_code ?? 0;
      if (startErr !== 0 || !sessionId || taskId === null) {
        markFailed(
          get,
          set,
          next.id,
          startErr,
          startResp.err_message ||
            startResp.detail ||
            `BGFT register failed (0x${startErr.toString(16)})`,
        );
        continue;
      }

      // Record session_id + task_id so cancel can reach it.
      {
        const items = get().items.map((it) =>
          it.id === next.id ? { ...it, sessionId, taskId } : it,
        );
        set({ items });
        persist(items);
      }

      // Poll until done or error.
      let phase: InstallPhase = "queued";
      let pollErrors = 0;
      while (isLive()) {
        await sleep(1000);
        if (!isLive()) return;
        let st: {
          phase?: InstallPhase;
          downloaded?: number;
          total?: number;
          err_code?: number;
          err_message?: string | null;
          detail?: string;
          cancelled?: boolean;
        };
        try {
          st = (await invoke("pkg_install_status", {
            session: sessionId,
          })) as typeof st;
          pollErrors = 0;
        } catch (e) {
          // Tolerate transient poll errors (PS5 swap, brief network
          // hiccup) up to 5 in a row before giving up.
          pollErrors += 1;
          if (pollErrors >= 5) {
            markFailed(get, set, next.id, 0, `status poll failed: ${e}`);
            break;
          }
          continue;
        }
        if (!isLive()) return;
        phase = st.phase ?? phase;
        const items = get().items.map((it) =>
          it.id === next.id
            ? {
                ...it,
                phase,
                bytesDownloaded: st.downloaded ?? it.bytesDownloaded,
                totalBytes: st.total ?? it.totalBytes,
                errCode: st.err_code ?? it.errCode,
                errMessage: st.err_message ?? it.errMessage,
              }
            : it,
        );
        set({ items });
        // Hot path — runs once per second per active install. Debounce
        // the localStorage write so we don't hammer storage. Terminal
        // transitions below (markDone/markFailed/markCancelled) call
        // `persist(items)` which flushes any pending debounced write,
        // so a sudden app exit doesn't lose the user's last status.
        persistDebounced(items);

        if (st.cancelled) {
          markCancelled(get, set, next.id);
          break;
        }
        if (phase === "done") {
          markDone(get, set, next.id);
          break;
        }
        if (phase === "error") {
          markFailed(
            get,
            set,
            next.id,
            st.err_code ?? 0,
            st.err_message ||
              st.detail ||
              `BGFT install failed (0x${(st.err_code ?? 0).toString(16)})`,
          );
          break;
        }
      }
      if (!isLive()) return;
    }

    set({ isRunning: false });
  },
}));

function markFailed(
  get: () => InstallQueueState,
  set: (s: Partial<InstallQueueState>) => void,
  id: string,
  errCode: number,
  errMessage: string,
) {
  const items = get().items.map((it) =>
    it.id === id
      ? {
          ...it,
          status: "failed" as InstallStatus,
          phase: "error" as InstallPhase,
          errCode,
          errMessage,
          finishedAt: Date.now(),
        }
      : it,
  );
  set({ items });
  persist(items);
}

function markDone(
  get: () => InstallQueueState,
  set: (s: Partial<InstallQueueState>) => void,
  id: string,
) {
  const items = get().items.map((it) =>
    it.id === id
      ? {
          ...it,
          status: "done" as InstallStatus,
          phase: "done" as InstallPhase,
          finishedAt: Date.now(),
        }
      : it,
  );
  set({ items });
  persist(items);

  // Tell the Library screen to refresh — a freshly-installed title
  // is now in /user/app/ and the user expects to see it the moment
  // they navigate to the Library tab. Library listens via
  // window.addEventListener("ps5upload:library:invalidate", ...).
  // CustomEvent works in browsers + Tauri webview without any extra
  // imports, and we don't need cross-window broadcasting since the
  // queue + Library live in the same renderer.
  if (typeof window !== "undefined") {
    try {
      window.dispatchEvent(new CustomEvent("ps5upload:library:invalidate"));
    } catch {
      /* noop — browsers/test envs without CustomEvent fall through */
    }
  }
}

function markCancelled(
  get: () => InstallQueueState,
  set: (s: Partial<InstallQueueState>) => void,
  id: string,
) {
  const items = get().items.map((it) =>
    it.id === id
      ? {
          ...it,
          status: "cancelled" as InstallStatus,
          finishedAt: Date.now(),
        }
      : it,
  );
  set({ items });
  persist(items);
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
