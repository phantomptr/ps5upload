// Per-host last-used mount destination (volume + subpath) for the
// Library Mount modal. Persisted to localStorage so reopening the
// Mount modal lands on the same destination the user picked last
// time — same UX as the Upload screen's destination, where the
// preset chip the user clicked sticks across sessions.
//
// Keyed by host because volume layouts differ between PS5s
// (a PRO with /mnt/ext1 + /mnt/usb0 vs. a Slim with only /data)
// and a remembered "/mnt/ext1" subpath would 502 if applied to the
// wrong console.

import { compareVersions } from "./semver";

const STORAGE_KEY = "ps5upload.mount.lastDest";

/** Default subpath. Matches today's mount root (`/mnt/ps5upload/`)
 *  conceptually — the legacy mount path is `/mnt/ps5upload/<name>`,
 *  i.e. an empty subpath under that volume. For user-chosen volumes
 *  (`/data`, `/mnt/ext1`) the `ps5upload` subpath replicates that
 *  layout: `/mnt/ext1/ps5upload/<name>`. Keeping it as the default
 *  preserves the principle of least surprise — fresh installs match
 *  prior payload versions. */
export const MOUNT_DEFAULT_SUBPATH = "ps5upload";

/** Same four labels Upload's `DestinationCard` uses, with mount-
 *  appropriate hints. Surfaced as preset chips in the Mount modal so
 *  users can one-click-pick the conventional layout for their flow. */
export const MOUNT_PRESETS: { label: string; subpath: string; hint: string }[] = [
  { label: "etaHEN/games", subpath: "etaHEN/games", hint: "Discoverable by etaHEN/GoldHen game scanners" },
  { label: "homebrew", subpath: "homebrew", hint: "Homebrew-app convention" },
  { label: "exfat", subpath: "exfat", hint: "Conventional disk-image holder" },
  { label: "ps5upload", subpath: "ps5upload", hint: "Legacy / generic — matches the pre-2.2.25 mount root" },
];

/** Volume roots where mounting is honored by 2.2.25+ payloads. The
 *  payload's `is_path_allowed` is the source of truth; this list
 *  mirrors the *real-disk* roots for the picker dropdown when the
 *  live `fetchVolumes` probe hasn't returned yet.
 *
 *  Deliberately excludes `/mnt/ps5upload` even though
 *  `is_path_allowed` accepts paths *under* it (`/mnt/ps5upload/foo`
 *  → ok). The payload rejects the namespace root itself with
 *  `fs_mount_bad_mount_point`, so showing it in the dropdown would
 *  surface a destination guaranteed to fail when the user clicks
 *  Mount. The default `ps5upload` subpath under any real volume
 *  (`/mnt/ext1/ps5upload/<name>`) gives the same effect without the
 *  rejection. */
const FALLBACK_VOLUMES = ["/data", "/mnt/ext0", "/mnt/usb0"];

export interface MountDest {
  volume: string;
  subpath: string;
}

type Map = Record<string, MountDest>;

function load(): Map {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Map = {};
    for (const [host, val] of Object.entries(parsed)) {
      if (
        typeof host === "string" &&
        val &&
        typeof val === "object" &&
        typeof (val as MountDest).volume === "string" &&
        typeof (val as MountDest).subpath === "string"
      ) {
        out[host] = {
          volume: (val as MountDest).volume,
          subpath: (val as MountDest).subpath,
        };
      }
    }
    return out;
  } catch {
    return {};
  }
}

function save(map: Map) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch {
    // localStorage write can fail (quota, private mode). Mount-dest
    // persistence is nice-to-have; swallow.
  }
}

/** Look up the last-used `{volume, subpath}` for `host`. Returns null
 *  when nothing has been saved yet — caller should fall back to a
 *  fresh-install default (typically: first available volume +
 *  MOUNT_DEFAULT_SUBPATH). */
export function loadMountDest(host: string): MountDest | null {
  if (!host?.trim()) return null;
  const map = load();
  return map[host.trim()] ?? null;
}

/** Persist `{volume, subpath}` as the last-used destination for
 *  `host`. No-op for empty hosts or empty volume strings (a mid-edit
 *  blank value shouldn't overwrite a real saved entry). */
export function saveMountDest(host: string, dest: MountDest): void {
  if (!host?.trim() || !dest.volume?.trim()) return;
  const map = load();
  const existing = map[host.trim()];
  if (existing && existing.volume === dest.volume && existing.subpath === dest.subpath) {
    return;
  }
  map[host.trim()] = { volume: dest.volume, subpath: dest.subpath };
  save(map);
}

/** Whether `payloadVersion` (as reported by STATUS_ACK) supports the
 *  2.2.25 mount_point field. Older payloads accept only `mount_name`
 *  and silently ignore `mount_point`, which would cause the user's
 *  picker selection to be honored in name only — leading to a mount
 *  at /mnt/ps5upload/<name> instead of the volume they picked. UI
 *  should disable the volume picker (or warn) when this returns
 *  false. */
export function payloadSupportsMountPoint(payloadVersion: string | null): boolean {
  if (!payloadVersion) return false;
  const cmp = compareVersions(payloadVersion, "2.2.25");
  return cmp !== null && cmp >= 0;
}

/** Volumes the picker shows when the live `fetchVolumes` probe
 *  hasn't returned yet (cold start, payload momentarily unreachable).
 *  Caller should prefer the live list when available. */
export function fallbackMountVolumes(): string[] {
  return [...FALLBACK_VOLUMES];
}

/** Compose a `{volume, subpath, name}` triple into the resolved mount
 *  path the payload will receive. Trims accidental leading/trailing
 *  slashes on `subpath` and `name`, and treats empty `subpath` as
 *  "mount directly under `volume`/<name>" (no intermediate dir). */
export function resolveMountPath(
  volume: string,
  subpath: string,
  name: string,
): string {
  const v = volume.replace(/\/+$/, "");
  const s = subpath.replace(/^\/+|\/+$/g, "");
  const n = name.replace(/^\/+|\/+$/g, "");
  if (!s) return `${v}/${n}`;
  return `${v}/${s}/${n}`;
}
