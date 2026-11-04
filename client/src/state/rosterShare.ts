/**
 * The self-hosted web UI's console list lives in the engine, so every browser that opens the page
 * sees the same consoles (a Docker container used from two computers showed each its own empty
 * list). The browser keeps its copy in localStorage as before, for the first paint; this module
 * keeps that copy and the engine's (`/api/shared/consoles`, engine shared_state.rs) the same.
 *
 * What is shared: each console's id, name, address, order, notes and Wake keys. What stays in each
 * browser: which console is selected, and the last-seen firmware and helper version (every open
 * page refreshes those on each status poll; sharing them would be a write per poll per browser).
 *
 * Saves name the revision they were made from. When another browser saved in between, the engine
 * answers with its copy, and the two are merged per console (each side's additions, removals and
 * edits since the last common copy) before saving again, so two people editing at once both keep
 * their change.
 *
 * Desktop and mobile apps keep their list on the device, and mirror it the same way to their own
 * engine, so `GET|PUT /api/consoles` (engine consoles_api.rs) sees and changes the list they show. On
 * first contact the two lists are merged rather than one replacing the other. An app pointed at
 * another computer's engine (Settings) does not mirror: that engine's list is someone else's.
 */
import { isTauriEnv } from "../lib/tauriEnv";
import { getEngineUrl, isLoopbackUrl } from "./engine";
import { useRosterStore, type PS5Profile } from "./roster";

/** A console as the engine keeps it: everything but this browser's last-seen cache. */
export type SharedProfile = Omit<PS5Profile, "last_seen_at" | "last_seen_kernel" | "last_seen_payload">;

export function sharedForm(profiles: readonly PS5Profile[]): SharedProfile[] {
  return profiles.map((p) => {
    const { last_seen_at: _a, last_seen_kernel: _k, last_seen_payload: _p, ...rest } = p;
    void _a;
    void _k;
    void _p;
    return rest;
  });
}

/** Key order and an empty field say nothing: the engine sends a profile's keys in its own order,
 *  and a cleared key comes back absent rather than null. */
function canonical(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(canonical);
  if (v && typeof v === "object") {
    const o = v as Record<string, unknown>;
    return Object.fromEntries(
      Object.keys(o)
        .filter((k) => o[k] !== null && o[k] !== undefined)
        .sort()
        .map((k) => [k, canonical(o[k])]),
    );
  }
  return v;
}

const same = (a: unknown, b: unknown) => JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));

/**
 * Three-way merge of console lists by id. `base` is the last copy both sides had, `mine` this
 * browser's, `theirs` the engine's. Their list is the starting point; then each of my changes
 * since `base` is applied: a console I added is appended, one I removed is dropped, one I edited
 * takes my version. If I reordered, my order wins for the consoles we both have.
 */
export function mergeShared(
  base: readonly SharedProfile[],
  mine: readonly SharedProfile[],
  theirs: readonly SharedProfile[],
): SharedProfile[] {
  const baseById = new Map(base.map((p) => [p.id, p]));
  const mineById = new Map(mine.map((p) => [p.id, p]));
  let out = theirs
    .filter((p) => !(baseById.has(p.id) && !mineById.has(p.id)))
    .map((p) => {
      const m = mineById.get(p.id);
      const b = baseById.get(p.id);
      return m && !same(m, b) ? m : p;
    });
  const have = new Set(out.map((p) => p.id));
  for (const p of mine) {
    if (!baseById.has(p.id) && !have.has(p.id)) out.push(p);
  }
  const mineOrder = mine.map((p) => p.id).filter((id) => baseById.has(id));
  const baseOrder = base.map((p) => p.id).filter((id) => mineById.has(id));
  if (!same(mineOrder, baseOrder)) {
    const rank = new Map(mine.map((p, i) => [p.id, i]));
    const ranked = out.filter((p) => rank.has(p.id)).sort((a, b) => rank.get(a.id)! - rank.get(b.id)!);
    let i = 0;
    out = out.map((p) => (rank.has(p.id) ? ranked[i++] : p));
  }
  return out;
}

interface Doc {
  rev: number;
  value: { profiles?: SharedProfile[] } | null;
}

/** The engine's side, swappable in tests. */
export interface ShareApi {
  get(): Promise<Doc>;
  /** The new revision, or the engine's current document when another save came first. */
  put(baseRev: number, profiles: SharedProfile[]): Promise<{ rev: number } | { conflict: Doc }>;
}

/** The app's own form of the list, with its keys (engine consoles_api.rs). */
const URL_PATH = "/api/consoles/document";

function shareBase(): string {
  const url = getEngineUrl();
  if (isTauriEnv() && !isLoopbackUrl(url)) throw new Error("the app is using another computer's engine");
  return url;
}

/**
 * The engine's answer, checked. Anything else (an older engine without the route answering some
 * other JSON, a proxy's page) is an error, so the browser's own list is never replaced by it: a
 * misread answer must not be able to erase someone's consoles.
 */
export function asDoc(v: unknown): Doc {
  const d = v as Partial<Doc> | null;
  const value = d?.value;
  const okValue =
    value === null || (typeof value === "object" && (value.profiles === undefined || Array.isArray(value.profiles)));
  if (!d || typeof d.rev !== "number" || !Number.isInteger(d.rev) || d.rev < 0 || !okValue) {
    throw new Error("the engine does not keep a shared console list");
  }
  return { rev: d.rev, value: value ?? null };
}

export const engineShareApi: ShareApi = {
  async get() {
    const r = await fetch(`${shareBase()}${URL_PATH}`);
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return asDoc(await r.json());
  },
  async put(baseRev, profiles) {
    const r = await fetch(`${shareBase()}${URL_PATH}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ base_rev: baseRev, value: { profiles } }),
    });
    if (r.status === 409) return { conflict: asDoc(await r.json()) };
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    return (await r.json()) as { rev: number };
  },
};

const profilesOf = (doc: Doc): SharedProfile[] =>
  Array.isArray(doc.value?.profiles) ? doc.value!.profiles! : [];

/**
 * Keeps the roster store and the engine's copy the same. `pull()` takes the engine's copy when
 * it changed; `push()` saves this browser's changes. Both run one at a time.
 */
export class RosterShare {
  /** The last copy this browser and the engine agreed on, and its revision. */
  private base: SharedProfile[] = [];
  private rev = 0;
  private busy: Promise<void> = Promise.resolve();
  /** Set while the store is being written from the engine, so that write is not saved back. */
  private applying = false;

  constructor(
    private readonly api: ShareApi,
    private readonly store = useRosterStore,
    /** "replace": on first contact the shared list wins (the Docker engine's list is the one list).
     *  "union": the two lists are merged, nothing dropped (a console's copy only holds what the
     *  devices that met it knew; 8.0.0 mesh). */
    private readonly firstContact: "replace" | "union" = "replace",
  ) {}

  private local(): SharedProfile[] {
    return sharedForm(this.store.getState().profiles);
  }

  private apply(profiles: SharedProfile[]) {
    if (same(profiles, this.local())) return;
    this.applying = true;
    try {
      this.store.getState().replaceShared(profiles as PS5Profile[]);
    } finally {
      this.applying = false;
    }
  }

  private queue(job: () => Promise<void>): Promise<void> {
    this.busy = this.busy.then(job, job);
    return this.busy;
  }

  /** First contact: the engine's list wins; a browser with consoles and an engine that has never
   *  had a list hands its consoles over (the list each browser kept before this shipped). */
  start(): Promise<void> {
    return this.queue(async () => {
      const doc = await this.api.get();
      if (doc.rev === 0 && this.local().length > 0) {
        this.rev = 0;
        this.base = [];
        await this.save();
        return;
      }
      this.rev = doc.rev;
      this.base = profilesOf(doc);
      if (this.firstContact === "union") {
        const merged = mergeShared([], this.local(), this.base);
        this.apply(merged);
        if (!same(merged, this.base)) await this.save();
        return;
      }
      this.apply(this.base);
    });
  }

  /** Takes the engine's list when another browser changed it. Local edits not yet saved are kept
   *  (merged in and saved). */
  pull(): Promise<void> {
    return this.queue(async () => {
      const doc = await this.api.get();
      if (doc.rev === this.rev) return;
      const theirs = profilesOf(doc);
      const mine = this.local();
      const merged = same(mine, this.base) ? theirs : mergeShared(this.base, mine, theirs);
      this.rev = doc.rev;
      this.base = theirs;
      this.apply(merged);
      if (!same(merged, theirs)) await this.save();
    });
  }

  /** Saves this browser's list when it differs from the last agreed copy. */
  push(): Promise<void> {
    return this.queue(() => this.save());
  }

  /** Whether a roster change came from the engine (and so needs no save). */
  get applyingRemote(): boolean {
    return this.applying;
  }

  private async save(): Promise<void> {
    for (let attempt = 0; attempt < 4; attempt++) {
      const mine = this.local();
      if (same(mine, this.base) && this.rev > 0) return;
      const r = await this.api.put(this.rev, mine);
      if ("rev" in r) {
        this.rev = r.rev;
        this.base = mine;
        return;
      }
      const theirs = profilesOf(r.conflict);
      const merged = mergeShared(this.base, mine, theirs);
      this.rev = r.conflict.rev;
      this.base = theirs;
      this.apply(merged);
    }
    throw new Error("the console list kept changing elsewhere; it will be saved on the next change");
  }
}

/** How often an open page looks for changes made in another browser (also on focus). */
export const PULL_MS = 30_000;

/** Starts sharing the console list with the engine; returns a stop. */
export function startRosterShare(api: ShareApi = engineShareApi): () => void {
  const share = new RosterShare(api, useRosterStore, isTauriEnv() ? "union" : "replace");
  const warn = (e: unknown) =>
    console.warn("[roster-share]", e instanceof Error ? e.message : String(e));
  let started = false;
  void share
    .start()
    .then(() => {
      started = true;
    })
    .catch(warn);
  const unsub = useRosterStore.subscribe((s, prev) => {
    if (!started || share.applyingRemote || s.profiles === prev.profiles) return;
    if (same(sharedForm(s.profiles), sharedForm(prev.profiles))) return; // last-seen only
    void share.push().catch(warn);
  });
  const pull = () => {
    if (!started) {
      void share
        .start()
        .then(() => {
          started = true;
        })
        .catch(warn);
      return;
    }
    if (document.visibilityState === "visible") void share.pull().catch(warn);
  };
  const timer = window.setInterval(pull, PULL_MS);
  window.addEventListener("focus", pull);
  document.addEventListener("visibilitychange", pull);
  return () => {
    unsub();
    window.clearInterval(timer);
    window.removeEventListener("focus", pull);
    document.removeEventListener("visibilitychange", pull);
  };
}
