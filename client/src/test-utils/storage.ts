// In-memory browser storage for tests: vitest's default environment has no `window`.
import { vi } from "vitest";

/** A `Storage` kept in memory, starting with `seed`. */
export function memoryStorage(seed: Record<string, string> = {}): Storage {
  const map = new Map(Object.entries(seed));
  return {
    getItem: (k: string) => map.get(k) ?? null,
    setItem: (k: string, v: string) => void map.set(k, String(v)),
    removeItem: (k: string) => void map.delete(k),
    clear: () => map.clear(),
    key: (i: number) => Array.from(map.keys())[i] ?? null,
    get length() {
      return map.size;
    },
  };
}

/** For code that reads the bare global `localStorage`. */
export function installLocalStorageStub(seed?: Record<string, string>): Storage {
  const storage = memoryStorage(seed);
  vi.stubGlobal("localStorage", storage);
  return storage;
}

/** For code that goes through `window.localStorage` (lib/safeStorage); `window` has nothing else. */
export function installWindowStub(seed?: Record<string, string>): Storage {
  const storage = memoryStorage(seed);
  vi.stubGlobal("window", { localStorage: storage });
  return storage;
}

/**
 * For a store that uses both spellings: guards on bare `localStorage` while lib/safeStorage goes
 * through `window.localStorage`. Stub only one and the reads work while the writes silently vanish.
 * The listener no-ops are there because such a store subscribes to window events on import.
 */
export function installStorage(): void {
  const local = memoryStorage();
  const session = memoryStorage();
  vi.stubGlobal("localStorage", local);
  vi.stubGlobal("sessionStorage", session);
  vi.stubGlobal("window", {
    localStorage: local,
    sessionStorage: session,
    addEventListener: () => {},
    removeEventListener: () => {},
  });
}
