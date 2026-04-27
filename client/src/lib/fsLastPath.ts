// Per-host last-browsed FileSystem path. Persisted to localStorage so
// reloading the app drops the user back where they were instead of at
// the hardcoded "/data" root.
//
// Keyed by host because most users have one PS5 but a small minority
// run multiple (a dev unit + a daily-driver, or two consoles in the
// same household). Storing per-host means switching the IP in the
// Connection screen restores *that* console's last directory rather
// than carrying state across mismatched volume layouts (the dev unit
// might mount /mnt/ext1 that the other console doesn't have).

const STORAGE_KEY = "ps5upload.fs.lastPath";
/** Hardcoded fallback when no localStorage entry exists for this host
 *  (first-time use of the app, or first-time use of a new IP).
 *  /data is always present on a stock PS5 and matches what FileSystem
 *  used as its initial path before persistence existed. */
export const FS_DEFAULT_PATH = "/data";

type Map = Record<string, string>;

function load(): Map {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    // Defensive: a malformed entry (manually edited, schema bump,
    // localStorage corruption) shouldn't crash the FileSystem
    // screen on mount. Drop bad shapes silently — the user falls
    // back to the default path, which is harmless.
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const out: Map = {};
      for (const [k, v] of Object.entries(parsed)) {
        if (typeof k === "string" && typeof v === "string") out[k] = v;
      }
      return out;
    }
    return {};
  } catch {
    return {};
  }
}

function save(map: Map) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch {
    // localStorage write can fail (quota exceeded, private mode).
    // Last-path memory is nice-to-have; swallow.
  }
}

/** Look up the last-browsed path for `host`. Returns FS_DEFAULT_PATH
 *  ("/data") when no entry exists, the host is empty/whitespace, or
 *  the stored value is empty. */
export function loadFsLastPath(host: string): string {
  if (!host?.trim()) return FS_DEFAULT_PATH;
  const map = load();
  const stored = map[host.trim()];
  return stored && stored.trim() ? stored : FS_DEFAULT_PATH;
}

/** Persist `path` as the last-browsed location for `host`. No-op for
 *  empty hosts or empty paths so we don't litter localStorage with
 *  meaningless entries when the connection input is mid-edit. */
export function saveFsLastPath(host: string, path: string): void {
  if (!host?.trim() || !path?.trim()) return;
  const map = load();
  if (map[host.trim()] === path) return; // skip redundant writes
  map[host.trim()] = path;
  save(map);
}
