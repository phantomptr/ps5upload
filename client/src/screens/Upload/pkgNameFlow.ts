// What the "Name on the PS5" field does (PackageNameCard), apart from how it looks: read a
// picked package's name, hold what the user types, and change the name in the package.

import type { PkgNames, PkgRenamed } from "../../api/pkgName";

export interface NameFlowState {
  /** The package the state is about; null before one is picked. */
  path: string | null;
  loading: boolean;
  /** The names as the package holds them now; null until read (or when it could not be). */
  names: PkgNames | null;
  /** What the field holds. */
  value: string;
  allLanguages: boolean;
  saving: boolean;
  /** The last change made to this package. */
  saved: PkgRenamed | null;
  /** Why the name could not be read (`read`) or changed (`save`), in the engine's words. */
  error: { during: "read" | "save"; reason: string } | null;
}

export interface NameFlowDeps {
  read: (path: string) => Promise<PkgNames>;
  set: (path: string, name: string, allLanguages: boolean) => Promise<PkgRenamed>;
  /** The package was changed: whoever shows its details reads them again. */
  renamed?: (path: string) => void;
}

const EMPTY: NameFlowState = {
  path: null,
  loading: false,
  names: null,
  value: "",
  allLanguages: false,
  saving: false,
  saved: null,
  error: null,
};

const reasonOf = (e: unknown) => (e instanceof Error ? e.message : String(e));

const utf8Bytes = (s: string) => new TextEncoder().encode(s).length;

/** The room the package has for the name in the field, in UTF-8 bytes: how many fit (`limit`:
 *  the package's own room for what would change, the default name alone or every language;
 *  the engine's overall limit when it gives no room), how many the name takes, and how many
 *  are left (negative: that many too long). null with no package read, or one that refuses. */
export function nameRoom(s: NameFlowState): { limit: number; used: number; left: number } | null {
  if (!s.names || s.names.refusal) return null;
  const { room, max_bytes, other_names } = s.names;
  const every = s.allLanguages && other_names > 0;
  const limit = Math.min(max_bytes, room ? (every ? room.all_languages : room.default_name) : max_bytes);
  const used = utf8Bytes(s.value.trim());
  return { limit, used, left: limit - used };
}

/** Why Change name is not offered for what the field holds: nothing typed, nothing to change
 *  (the same name, unless every language is asked for and some differ), a name longer than the
 *  package has room for (counted in bytes, as the engine counts: said in the field itself,
 *  before anything is sent), the package refuses a rename, or it is being read or written.
 *  null when it can be pressed. */
export function saveBlocked(
  s: NameFlowState,
): "empty" | "same" | "too_long" | "refused" | "busy" | null {
  if (!s.names || s.loading || s.saving) return "busy";
  if (s.names.refusal) return "refused";
  const name = s.value.trim();
  if (!name) return "empty";
  const changesOthers = s.allLanguages && s.names.other_names > 0;
  if (name === s.names.name && !changesOthers) return "same";
  if ((nameRoom(s)?.left ?? 0) < 0) return "too_long";
  return null;
}

export interface NameFlow {
  get: () => NameFlowState;
  subscribe: (listener: () => void) => () => void;
  /** Read `path`'s name (null: no package). An answer for a package picked before is dropped. */
  load: (path: string | null) => Promise<void>;
  setValue: (value: string) => void;
  setAllLanguages: (all: boolean) => void;
  /** Change the package's name to what the field holds. */
  save: () => Promise<void>;
}

export function createNameFlow(deps: NameFlowDeps): NameFlow {
  let state = EMPTY;
  const listeners = new Set<() => void>();
  const put = (next: Partial<NameFlowState>) => {
    state = { ...state, ...next };
    for (const l of listeners) l();
  };
  return {
    get: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    async load(path) {
      if (!path) {
        put(EMPTY);
        return;
      }
      put({ ...EMPTY, path, loading: true });
      try {
        const names = await deps.read(path);
        if (state.path !== path) return;
        put({ loading: false, names, value: names.name });
      } catch (e) {
        if (state.path !== path) return;
        put({ loading: false, error: { during: "read", reason: reasonOf(e) } });
      }
    },
    setValue(value) {
      put({ value, saved: null, error: state.error?.during === "save" ? null : state.error });
    },
    setAllLanguages(allLanguages) {
      put({ allLanguages, saved: null });
    },
    async save() {
      const path = state.path;
      if (!path || saveBlocked(state)) return;
      const name = state.value.trim();
      put({ saving: true, error: null, saved: null });
      try {
        const saved = await deps.set(path, name, state.allLanguages);
        if (state.path !== path) return;
        // The package as it is now, read again: the names it holds are the ones to show.
        let names = state.names;
        try {
          names = await deps.read(path);
        } catch {
          // The change is made; an unread name only leaves the field showing what was typed.
        }
        if (state.path !== path) return;
        put({ saving: false, saved, names, value: names?.name ?? saved.name, allLanguages: false });
        deps.renamed?.(path);
      } catch (e) {
        if (state.path !== path) return;
        put({ saving: false, error: { during: "save", reason: reasonOf(e) } });
      }
    },
  };
}
