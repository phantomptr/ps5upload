// Tauri IPC wrappers — call the Rust-side `#[tauri::command]` handlers
// registered in `client/src-tauri/src/lib.rs`. Prefer this surface over
// `api/engine.ts` for desktop operations: no HTTP roundtrip, no CSP
// allowlisting, and host-local calls (folder inspection, ELF loader
// TCP send) stay in-process.

import { invoke } from "@tauri-apps/api/core";

export interface FolderInspectResult {
  path: string;
  title: string | null;
  title_id: string | null;
  content_id: string | null;
  content_version: string | null;
  application_category_type: number | null;
  icon0_path: string | null;
  total_size: number;
  file_count: number;
  skipped_paths: string[];
  meta_source: "param.json" | "param.sfo" | "none";
}

/** Returned alongside a root inspect when the root is not a game but a
 *  single child subdir is — the Upload screen surfaces a "did you mean
 *  `<child>`?" chip using this. See `folder_inspect.rs::wrapped_game_hint`. */
export interface WrappedGameHint {
  path: string;
  title: string | null;
  title_id: string | null;
}

export interface FolderInspection {
  result: FolderInspectResult;
  wrapped_hint: WrappedGameHint | null;
}

type InspectFolderResponse =
  | { ok: true; result: FolderInspectResult; wrapped_hint: WrappedGameHint | null }
  | { ok: false; error: string };

/**
 * Walk a local folder and extract a preview card for the Upload screen:
 * title, title_id, content_id, disk size, file count, icon0 path.
 *
 * Wraps `ps5upload_core::game_meta::inspect_folder`. Runs on a blocking
 * tokio thread so the renderer stays responsive during deep walks.
 */
/** "file" | "folder" | "other" | "missing". Used by the Upload screen's
 *  drag-drop handler to route the dropped path into the right picker. */
export async function pathKind(
  path: string
): Promise<"file" | "folder" | "other" | "missing"> {
  const res = await invoke<{ kind: string }>("path_kind", { path });
  const k = res.kind;
  if (k === "file" || k === "folder" || k === "other" || k === "missing") return k;
  return "missing";
}

export async function inspectFolder(path: string): Promise<FolderInspection> {
  const res = await invoke<InspectFolderResponse>("inspect_folder", { path });
  if (!res.ok) throw new Error(res.error);
  return { result: res.result, wrapped_hint: res.wrapped_hint };
}

/**
 * Send the payload ELF to the PS5's ELF loader.
 *
 * `port` defaults to 9021 — the shared elfldr convention every
 * mainstream PS5 loader binds. Pass an override only if you're
 * targeting a custom loader that listens on a different port. Wraps
 * `commands::payload_send` in Rust.
 */
export async function sendPayload(
  ip: string,
  elfPath: string,
  port?: number
): Promise<void> {
  const resp = await invoke<{ ok?: boolean; status?: string; error?: string }>(
    "payload_send",
    { ip, path: elfPath, port: port ?? null }
  );
  if (resp && resp.ok === false) {
    throw new Error(resp.error ?? resp.status ?? "payload_send failed");
  }
}

// ─── Transfer jobs ────────────────────────────────────────────────────────

/** Start a single-file upload. Returns the job id; poll `jobStatus(id)`
 *  until terminal. `txId` is optional — pass a stable hex string when you
 *  want the engine to reuse an earlier tx (for resume); omit it to let
 *  the engine mint a fresh one. */
export async function startTransferFile(
  src: string,
  dest: string,
  addr: string,
  txId?: string | null,
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_file", {
    req: { src, dest, addr, tx_id: txId ?? null },
  });
  return res.job_id;
}

/** Start a recursive directory upload. `destRoot` is the parent dir on
 *  the PS5 under which `srcDir`'s *contents* land. Overwrites whatever
 *  is already there — use `startTransferDirReconcile` for resume. */
export async function startTransferDir(
  srcDir: string,
  destRoot: string,
  addr: string,
  txId?: string | null,
  excludes?: string[],
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_dir", {
    req: {
      src_dir: srcDir,
      dest_root: destRoot,
      addr,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
    },
  });
  return res.job_id;
}

export type ReconcileMode = "fast" | "safe";

/** Resume-friendly folder upload. The engine walks the PS5 destination,
 *  diffs by size (fast) or BLAKE3 hash (safe), then only sends the
 *  delta. Identical API to `startTransferDir` otherwise. Progress bar
 *  ends up tracking only what actually needs sending.
 *
 *  `txId` drives cross-session shard-level resume: pass the tx_id that
 *  was used on the interrupted attempt, and the engine will set
 *  `TX_FLAG_RESUME` on its retries so the payload can surface
 *  `last_acked_shard` from its journal. Without this, resume is
 *  limited to file-level reconcile (skip already-present files). */
export async function startTransferDirReconcile(
  srcDir: string,
  destRoot: string,
  addr: string,
  mode: ReconcileMode,
  txId?: string | null,
  excludes?: string[],
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_dir_reconcile", {
    req: {
      src_dir: srcDir,
      dest_root: destRoot,
      addr,
      mode,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
    },
  });
  return res.job_id;
}

// ─── Cross-session resume tx_id persistence ─────────────────────────────
//
// The Tauri side keeps a JSON store of (host, src, dest) → tx_id_hex so
// the client can offer a real shard-level resume across app restarts,
// not just within-session retry. 24h TTL on each record; expired entries
// are pruned on lookup. See src-tauri/src/commands/persistence.rs for
// the schema.

/** Look up the tx_id we registered for this (host, src, dest) on a
 *  prior upload attempt. Returns null when no non-expired record exists;
 *  caller should generate a fresh tx_id in that case. */
export async function resumeTxidLookup(
  host: string,
  src: string,
  dest: string,
): Promise<string | null> {
  const res = await invoke<{ tx_id_hex: string | null }>("resume_txid_lookup", {
    req: { host, src, dest },
  });
  return res.tx_id_hex ?? null;
}

/** Register the tx_id the client is about to send to the engine. Called
 *  at upload start so a later Resume click can recover it. */
export async function resumeTxidRemember(
  host: string,
  src: string,
  dest: string,
  txIdHex: string,
  mode: "reconcile" | "dir" | "file",
): Promise<void> {
  await invoke("resume_txid_remember", {
    req: { host, src, dest, tx_id_hex: txIdHex, mode },
  });
}

/** Drop the tx_id record for (host, src, dest). Called on successful
 *  Done (nothing to resume) and on explicit Override (user is starting
 *  over; keeping the stale tx_id would skip shards on the next Resume
 *  click that shouldn't be skipped). */
export async function resumeTxidForget(
  host: string,
  src: string,
  dest: string,
): Promise<void> {
  await invoke("resume_txid_forget", {
    req: { host, src, dest },
  });
}

/** Generate a fresh 16-byte tx_id as a 32-char lowercase hex string.
 *  Matches the format the engine's `parse_or_random_tx_id` accepts.
 *  Uses Web Crypto's UUIDv4 generator for a CSPRNG source — collisions
 *  across the payload's tx table would corrupt other uploads, so "good
 *  random" actually matters here. */
/** Start a PS5 → host download job. `kind` is the caller's known
 *  classification — Library/FileSystem rows already know whether
 *  they're showing a file or a folder, so we trust the hint and
 *  skip a remote stat round-trip. Returns the engine job_id; poll
 *  via `jobStatus` to track progress + terminal state. */
export async function startTransferDownload(
  srcPath: string,
  destDir: string,
  transferAddr: string,
  kind: "file" | "folder",
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_download", {
    req: {
      src_path: srcPath,
      dest_dir: destDir,
      addr: transferAddr,
      kind,
    },
  });
  return res.job_id;
}

/** Whole-document load for the upload-queue store. The renderer owns
 *  the shape — see state/uploadQueue.ts. Returns `{}` for first-time
 *  use (Tauri returns the empty default from persistence.rs). */
export async function uploadQueueLoad<T = unknown>(): Promise<T> {
  return invoke<T>("upload_queue_load");
}

/** Whole-document save for the upload-queue store. Caller is
 *  responsible for serializing concurrent saves; the Tauri side has a
 *  per-store mutex so worst case is ordering, not corruption. */
export async function uploadQueueSave(doc: unknown): Promise<void> {
  await invoke("upload_queue_save", { doc });
}

/** Whole-document load for the payload-playlist store. */
export async function payloadPlaylistsLoad<T = unknown>(): Promise<T> {
  return invoke<T>("payload_playlists_load");
}

/** Whole-document save for the payload-playlist store. */
export async function payloadPlaylistsSave(doc: unknown): Promise<void> {
  await invoke("payload_playlists_save", { doc });
}

export function generateTxIdHex(): string {
  // crypto.randomUUID() returns "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx"
  // (36 chars with dashes). Stripping the 4 dashes yields exactly 32
  // hex chars = 16 bytes. The Version-4 and variant bits burn ~6 bits
  // of entropy but 122 random bits is still far more than we need for
  // uniqueness within the PS5's tx table (max 32-ish live entries).
  return crypto.randomUUID().replace(/-/g, "");
}

export interface DestinationProbe {
  exists: boolean;
  entryCount: number;
  totalBytes: number;
}

export interface Volume {
  path: string;
  mount_from?: string;
  fs_type: string;
  total_bytes: number;
  free_bytes: number;
  writable: boolean;
  is_placeholder?: boolean;
  /** For `/mnt/ps5upload/*` mounts, the backing image file. Empty
   *  string on storage drives or on mounts predating the tracker. */
  source_image?: string;
}

export interface SearchHit {
  path: string;
  name: string;
  size: number;
  kind: string;
}

/** Client-side recursive search. Walks writable volumes via FS_LIST_DIR,
 *  filters entries whose names match the glob (wildcards `*` and `?`).
 *  Slow for huge trees (every dir = one FS_LIST_DIR round-trip), but
 *  zero payload-side infrastructure required. `minSize` filters out
 *  entries smaller than the threshold (bytes); 0 = no filter.
 *
 *  Safety: caps total entries scanned + max depth so a pathological
 *  input can't peg the app. Returns partial results with a `truncated`
 *  flag when the budget is exhausted. */
export interface SearchProgress {
  scanned: number;
  hits: number;
  currentPath: string;
}

/** Additional result field: `cancelled` is true when the caller aborted
 *  mid-scan via the AbortSignal. Distinct from `truncated` (caller-level
 *  limit hit): callers want to know why they got partial results. */
export async function searchPS5(
  transferAddr: string,
  pattern: string,
  minSize: number,
  onProgress?: (p: SearchProgress) => void,
  roots?: string[],
  signal?: AbortSignal
): Promise<{
  hits: SearchHit[];
  scanned: number;
  truncated: boolean;
  cancelled: boolean;
}> {
  const addr = toMgmtAddr(transferAddr);
  const MAX_ENTRIES_SCANNED = 100_000;
  const MAX_DEPTH = 16;

  // Default roots = every writable non-placeholder volume.
  let scanRoots: string[];
  if (roots && roots.length > 0) {
    scanRoots = roots;
  } else {
    const volRes = await invoke<{ volumes?: Volume[] }>("ps5_volumes", { addr });
    scanRoots = (volRes?.volumes ?? [])
      .filter((v) => v.writable && !v.is_placeholder)
      .map((v) => v.path);
  }

  // Convert glob to regex. `*` → `.*`, `?` → `.`, rest escaped.
  const regex = patternToRegex(pattern);

  const hits: SearchHit[] = [];
  let scanned = 0;
  let truncated = false;
  let cancelled = false;

  const stack: { path: string; depth: number }[] = scanRoots.map((r) => ({
    path: r,
    depth: 0,
  }));

  // Throttle progress callbacks so we don't thrash React state for every
  // entry on huge trees. 100 ms cadence is fast enough for a live
  // feel, slow enough to coalesce bursts from `list_dir` returning
  // dozens of entries per call.
  let lastEmit = 0;
  const maybeEmit = (currentPath: string) => {
    if (!onProgress) return;
    const now = Date.now();
    if (now - lastEmit < 100) return;
    lastEmit = now;
    onProgress({ scanned, hits: hits.length, currentPath });
  };

  while (stack.length > 0) {
    if (signal?.aborted) {
      cancelled = true;
      break;
    }
    if (scanned >= MAX_ENTRIES_SCANNED) {
      truncated = true;
      break;
    }
    const { path, depth } = stack.pop()!;
    if (depth > MAX_DEPTH) continue;
    maybeEmit(path);
    let listing: { entries?: Array<{ name?: string; kind?: string; size?: number }> };
    try {
      listing = await invoke("ps5_list_dir", {
        addr,
        path,
        offset: 0,
        limit: 256,
      });
    } catch {
      continue;
    }
    for (const e of listing.entries ?? []) {
      scanned += 1;
      if (!e.name) continue;
      const full = path.endsWith("/") ? `${path}${e.name}` : `${path}/${e.name}`;
      if (e.kind === "dir") {
        stack.push({ path: full, depth: depth + 1 });
      }
      const size = typeof e.size === "number" ? e.size : 0;
      if (size < minSize) continue;
      if (!regex.test(e.name)) continue;
      hits.push({
        path: full,
        name: e.name,
        size,
        kind: e.kind ?? "unknown",
      });
    }
  }
  // Final emit so UI reflects the full count even if we skipped the
  // last throttled update.
  if (onProgress) onProgress({ scanned, hits: hits.length, currentPath: "" });

  return { hits, scanned, truncated, cancelled };
}

function patternToRegex(pattern: string): RegExp {
  if (!pattern.trim()) return /.*/i; // empty pattern matches everything
  const escaped = pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&");
  const globbed = escaped.replace(/\*/g, ".*").replace(/\?/g, ".");
  return new RegExp(`^${globbed}$`, "i");
}

// ─── Destructive FS ops ─────────────────────────────────────────────────────
//
// All use the mgmt port. The payload enforces a writable-root allowlist
// (/data/**, /user/**, /mnt/ext*/**, /mnt/usb*/**) — other paths return
// a `*_path_not_allowed` error surfaced here as an Error thrown to the UI.

export async function fsDelete(transferAddr: string, path: string): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_delete", { req: { addr, path } });
}

export async function fsMove(
  transferAddr: string,
  from: string,
  to: string,
  opId?: number,
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_move", {
    req: { addr, from, to, op_id: opId ?? 0 },
  });
}

export async function fsChmod(
  transferAddr: string,
  path: string,
  mode: string,
  recursive: boolean
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_chmod", { req: { addr, path, mode, recursive } });
}

export async function fsMkdir(transferAddr: string, path: string): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_mkdir", { req: { addr, path } });
}

/** Recursive copy on the PS5. Works across mount points (unlike fsMove,
 *  which is rename()-backed and fails with EXDEV across volumes). The
 *  payload refuses to overwrite an existing `to` — callers should
 *  pre-check via list_dir or be ready to surface a "dest_exists" error. */
export async function fsCopy(
  transferAddr: string,
  from: string,
  to: string,
  opId?: number,
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_copy", {
    req: { addr, from, to, op_id: opId ?? 0 },
  });
}

/** Snapshot the in-flight FS op identified by `opId`. Returns the
 *  payload's bytes_copied/total_bytes/cancel_requested. Used by the
 *  paste loop to drive a per-byte progress bar while fsCopy is
 *  blocked on FS_COPY_ACK. Throws if the op_id is no longer
 *  registered (op finished or never started) — caller should treat
 *  that as "stop polling, the op is done". */
export interface FsOpStatusSnapshot {
  op_id: number;
  kind: string;
  from: string;
  to: string;
  total_bytes: number;
  bytes_copied: number;
  cancel_requested: boolean;
}

export async function fsOpStatus(
  transferAddr: string,
  opId: number,
): Promise<FsOpStatusSnapshot> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<FsOpStatusSnapshot>("ps5_fs_op_status", {
    req: { addr, op_id: opId },
  });
}

/** Ask the payload to set the cancel flag on op `opId`. The payload's
 *  cp_rf loop checks the flag every 4 MiB, so a multi-GiB copy stops
 *  within ~one disk-IO of the cancel call. Returns true if the op
 *  was running, false if it had already finished — both are fine
 *  outcomes from the caller's POV. */
export async function fsOpCancel(
  transferAddr: string,
  opId: number,
): Promise<boolean> {
  const addr = toMgmtAddr(transferAddr);
  const res = await invoke<{ cancelled: boolean }>("ps5_fs_op_cancel", {
    req: { addr, op_id: opId },
  });
  return res.cancelled;
}

/** Result of a successful FS_MOUNT — the payload echoes where it mounted
 *  the image, which dev node it used, and the filesystem it chose. */
export interface MountResult {
  mount_point: string;
  dev_node: string;
  fstype: string;
}

/** Mount a disk image (.exfat or .ffpkg) on the PS5 via the payload's
 *  built-in MD-attach + nmount pipeline. `mountName` is optional — the
 *  payload derives a filesystem-safe name from the image basename when
 *  omitted. Images always land under `/mnt/ps5upload/<name>/`. */
export async function fsMount(
  transferAddr: string,
  imagePath: string,
  mountName?: string
): Promise<MountResult> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<MountResult>("ps5_fs_mount", {
    req: { addr, image_path: imagePath, mount_name: mountName ?? null },
  });
}

/** Unmount a previously-mounted image. `mountPoint` must be the exact
 *  path returned from `fsMount` — the payload rejects anything outside
 *  `/mnt/ps5upload/`. */
export async function fsUnmount(
  transferAddr: string,
  mountPoint: string
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_unmount", { req: { addr, mount_point: mountPoint } });
}

// ─── Hardware monitoring ──────────────────────────────────────────────
// Hardware monitoring — HW_INFO / HW_TEMPS / HW_POWER. Static info
// is cached server-side forever; live sensors have a 1s payload
// cache and a stricter 5s throttle on the power API.

export interface HwInfo {
  model: string;
  serial: string;
  has_wlan_bt: boolean;
  has_optical_out: boolean;
  hw_model: string;
  hw_machine: string;
  os: string;
  ncpu: number;
  /** bytes */
  physmem: number;
}

export interface HwTemps {
  /** celsius */
  cpu_temp: number;
  /** celsius */
  soc_temp: number;
  cpu_freq_mhz: number;
  soc_clock_mhz: number;
  /** milliwatts */
  soc_power_mw: number;
}

export interface HwPower {
  operating_time_sec: number;
  operating_time_hours: number;
  operating_time_minutes: number;
  boot_count: number;
  power_consumption_mw: number;
}

export async function fetchHwInfo(transferAddr: string): Promise<HwInfo> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwInfo>("ps5_hw_info", { addr });
}

export async function fetchHwTemps(transferAddr: string): Promise<HwTemps> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwTemps>("ps5_hw_temps", { addr });
}

export async function fetchHwPower(transferAddr: string): Promise<HwPower> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwPower>("ps5_hw_power", { addr });
}

// ─── Scene-tool companion probe ───────────────────────────────────────
// Not a payload RPC — this is a pure Rust-side network probe that does
// a TCP connect on each of a hardcoded list of well-known scene-tool
// ports. Fast, no protocol handshake. Feeds the companion chip strip
// on the Connection screen so users can see at a glance which scene
// tools are alive on their PS5 right now.

export interface CompanionStatus {
  name: string;
  role: string;
  port: number;
  reachable: boolean;
  error?: string;
}

export async function probeCompanions(
  host: string,
): Promise<CompanionStatus[]> {
  return invoke<CompanionStatus[]>("companion_probe", { host });
}

/** Bounds mirror the payload's clamp (payload/include/hw_info.h) and
 *  the engine's range check (engine/crates/ps5upload-core/src/hw.rs).
 *  Keeping them in sync here lets the UI prevent out-of-range
 *  requests before they ever hit the engine round-trip. */
export const FAN_THRESHOLD_MIN_C = 45;
export const FAN_THRESHOLD_MAX_C = 80;

/** Set the PS5's fan-turbo threshold in °C. `/dev/icc_fan` ioctl —
 *  the canonical fan-threshold mechanism on PS5. Persists until the
 *  PS5 reboots — there is no read-back, so we can't verify the current
 *  setting, only write new ones. */
export async function setFanThreshold(
  transferAddr: string,
  thresholdC: number,
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_hw_set_fan_threshold", {
    addr,
    thresholdC: Math.round(thresholdC),
  });
}

export type LibraryKind = "game" | "image";

export interface LibraryEntry {
  kind: LibraryKind;
  /** Display name (folder name for games, filename for disk images). */
  name: string;
  /** Absolute PS5 path. */
  path: string;
  /** Volume this entry lives on (e.g. "/data", "/mnt/ext1"). */
  volume: string;
  /** Scan suffix this entry was found under (e.g. "homebrew"). */
  scope: string;
  /** Size in bytes. For folders, the size the PS5 reported for the
   *  directory entry itself (usually 0 — not the recursive total). */
  size: number;
  /** For `image` kind only: the disk image type derived from the file
   *  extension. `exfat` = .exfat (exFAT volume), `ffpkg` = .ffpkg
   *  (UFS2 volume). Null for game entries. Drives the label + mount
   *  behavior in the Library row. */
  imageFormat?: "exfat" | "ffpkg" | null;
  /** For `game` kind: PS5 title id (e.g. "PPSA00000") read from the
   *  game's sce_sys/param.json during scan. Used to dedup rows when
   *  the same game appears at multiple paths -- typically when a
   *  disk image is mounted AND its backing folder is also accessible
   *  on the filesystem. */
  titleId?: string | null;
}

/** Walk every writable PS5 volume recursively and return all games
 *  and disk images found anywhere on them.
 *
 *  Detection rules:
 *   - **Game folder** = a directory whose `sce_sys/param.json` exists
 *     and is a regular file. Matches Sony's own convention; robust
 *     against extracted fragments since fragments don't carry
 *     `sce_sys/param.json`.
 *   - **Disk image** = a file whose name (case-insensitive) ends in
 *     `.exfat` or `.ffpkg`.
 *
 *  Determinism guarantees:
 *   - Listings are sorted by (kind, name) before they drive traversal
 *     or output, so the PS5 returning entries in inode order vs
 *     alphabetical doesn't change what we find or how we walk.
 *   - Entries are deduplicated by absolute path via a Map, so the
 *     same game can't be counted twice even if a symlink loop or
 *     bind-mount re-exposes it on a different route.
 *   - `visited` set short-circuits cycles: any path we've already
 *     descended into doesn't get re-walked.
 *   - Entry cap is deliberately high (200 k per volume) — earlier
 *     values (50 k) cut off on populated drives and produced
 *     different counts across refreshes depending on walk order.
 *
 *  Skipped subtrees (don't contain user-visible library content):
 *   - `/data/ps5upload/` — our own transfer journal / spool / tx-log
 *   - dot-prefixed directories (`.git/`, `.cache/`, …) — hidden tool state
 *   - `sce_sys/`, `sce_module/`, `prx/` at any depth — these are game
 *     internals; if we're seeing them at the top of a walk the user
 *     has messy data, but recursing in won't yield games anyway
 *
 *  Permission-denied / non-existent paths are silently skipped — the
 *  PS5 has plenty of system-only directories a user payload can't
 *  read, and they're not library content. */
export async function scanLibrary(transferAddr: string): Promise<LibraryEntry[]> {
  const addr = toMgmtAddr(transferAddr);
  const volumesRaw = await invoke<{ volumes?: Volume[] }>("ps5_volumes", { addr });
  const volumes = (volumesRaw?.volumes ?? []).filter(
    (v) => v.writable && !v.is_placeholder
  );

  const MAX_DEPTH = 8;
  // Deliberately high — lower caps (50 k) used to bail out mid-walk on
  // populated drives, producing different game counts on each refresh
  // depending on which branch of the DFS got visited first.
  const MAX_ENTRIES_PER_VOLUME = 200_000;
  const SKIP_BASENAMES = new Set([
    "sce_sys",
    "sce_module",
    "prx",
    "system",
    "system_data",
    "system_ex",
    "preinst",
    "update",
  ]);
  const SKIP_ABS_PATHS = new Set(["/data/ps5upload"]);

  // Dedupe by absolute path. A Map (rather than pushing into an
  // array) keeps the last observation and silently drops re-finds,
  // so symlink loops or multiple volumes exposing the same mount
  // converge on a single row.
  const byPath = new Map<string, LibraryEntry>();
  // Track visited directories so walker cycles terminate. Keyed by
  // absolute path; values don't matter.
  const visited = new Set<string>();

  const scanLimit = createScanLimiter(6);

  const listDir = async (
    path: string
  ): Promise<Array<{ name?: string; kind?: string; size?: number }>> =>
    scanLimit(async () => {
      try {
        const res = await invoke<{
          entries?: Array<{ name?: string; kind?: string; size?: number }>;
        }>("ps5_list_dir", { addr, path, offset: 0, limit: 512 });
        const out = res.entries ?? [];
        // Stable sort: dir/file kind first, then name. Eliminates
        // non-determinism from the payload returning entries in
        // inode or readdir order instead of alphabetical.
        out.sort((a, b) => {
          const ak = a.kind ?? "";
          const bk = b.kind ?? "";
          if (ak !== bk) return ak.localeCompare(bk);
          return (a.name ?? "").localeCompare(b.name ?? "");
        });
        return out;
      } catch {
        return [];
      }
    });

  const walkVolume = async (volumePath: string): Promise<void> => {
    let scanned = 0;
    const stack: Array<{ path: string; depth: number }> = [
      { path: volumePath, depth: 0 },
    ];
    while (stack.length > 0) {
      if (scanned >= MAX_ENTRIES_PER_VOLUME) break;
      const frame = stack.pop();
      if (!frame) break;
      const { path, depth } = frame;
      if (depth > MAX_DEPTH) continue;
      if (SKIP_ABS_PATHS.has(path)) continue;
      if (visited.has(path)) continue;
      visited.add(path);

      const listing = await listDir(path);
      scanned += listing.length;

      const dirNames: string[] = [];
      for (const e of listing) {
        if (!e.name) continue;
        if (e.kind === "file") {
          const lower = e.name.toLowerCase();
          const imageFormat: "exfat" | "ffpkg" | null = lower.endsWith(
            ".exfat"
          )
            ? "exfat"
            : lower.endsWith(".ffpkg")
              ? "ffpkg"
              : null;
          if (imageFormat) {
            const full = path === "/" ? `/${e.name}` : `${path}/${e.name}`;
            byPath.set(full, {
              kind: "image",
              name: e.name,
              path: full,
              volume: volumePath,
              scope: path,
              size: typeof e.size === "number" ? e.size : 0,
              imageFormat,
            });
          }
        } else if (e.kind === "dir") {
          dirNames.push(e.name);
        }
      }

      const hasSceSys = dirNames.includes("sce_sys");
      if (hasSceSys) {
        const sceSysPath = path === "/" ? "/sce_sys" : `${path}/sce_sys`;
        const sceContents = await listDir(sceSysPath);
        const hasParamJson = sceContents.some(
          (c) => c.name === "param.json" && c.kind === "file"
        );
        if (hasParamJson) {
          const basename =
            path === volumePath
              ? volumePath.split("/").pop() || volumePath
              : path.split("/").pop() || path;
          byPath.set(path, {
            kind: "game",
            name: basename,
            path,
            volume: volumePath,
            scope: path.substring(0, path.lastIndexOf("/")) || "/",
            size: 0,
          });
          continue;
        }
      }

      // Push children in reverse sorted order so stack.pop() yields
      // them in ascending alphabetical order — makes the walk order
      // predictable and the entry-cap cutoff deterministic.
      for (let i = dirNames.length - 1; i >= 0; i--) {
        const name = dirNames[i];
        if (name.startsWith(".")) continue;
        if (SKIP_BASENAMES.has(name)) continue;
        const child = path === "/" ? `/${name}` : `${path}/${name}`;
        if (SKIP_ABS_PATHS.has(child)) continue;
        if (visited.has(child)) continue;
        stack.push({ path: child, depth: depth + 1 });
      }
    }
  };

  await Promise.all(volumes.map((v) => walkVolume(v.path)));

  // Dedup-by-title-id pass: a game folder may surface at multiple
  // paths when an .exfat image is mounted (e.g., the game appears
  // BOTH at its original /data/homebrew/<id> folder AND at the
  // /mnt/ps5upload/<mountname>/ nullfs view). Without dedup, the
  // Library shows two rows with identical Install buttons -- if the
  // user installs from the wrong one, the registration state is
  // confusing. Fetch title_id for each game and keep one path per
  // title_id, preferring the /mnt/ps5upload/* variant since that's
  // the PS5-native mount form that Sony's launcher expects for
  // nullfs-backed titles.
  const rawGames = Array.from(byPath.values()).filter(
    (e) => e.kind === "game"
  );
  const rawNonGames = Array.from(byPath.values()).filter(
    (e) => e.kind !== "game"
  );
  // Fetch title_ids concurrently, capped by the same 6-slot limiter
  // the walker uses. 20ish games ~= ~200ms total on LAN.
  const titleIdLimit = createScanLimiter(6);
  const withTitleIds = await Promise.all(
    rawGames.map(async (game) => {
      const tid = await titleIdLimit(async () => {
        const meta = await fetchGameMeta(addr, game.path);
        return meta.title_id ?? null;
      });
      return { ...game, titleId: tid };
    })
  );
  // Group by titleId, pick the preferred path per group.
  const byTitleId = new Map<string, LibraryEntry[]>();
  const noTitleId: LibraryEntry[] = [];
  for (const g of withTitleIds) {
    if (!g.titleId) {
      noTitleId.push(g);
      continue;
    }
    const list = byTitleId.get(g.titleId) ?? [];
    list.push(g);
    byTitleId.set(g.titleId, list);
  }
  const dedupedGames: LibraryEntry[] = [...noTitleId];
  for (const [, candidates] of byTitleId) {
    if (candidates.length === 1) {
      dedupedGames.push(candidates[0]);
      continue;
    }
    // Prefer a mount-backed path; fall back to alphabetical first.
    const mountBacked = candidates.find((c) =>
      c.path.startsWith("/mnt/ps5upload/")
    );
    dedupedGames.push(mountBacked ?? candidates.sort((a, b) => a.path.localeCompare(b.path))[0]);
  }
  const entries = [...dedupedGames, ...rawNonGames];
  entries.sort((a, b) => {
    if (a.kind !== b.kind) {
      const order: Record<LibraryKind, number> = { game: 0, image: 1 };
      return order[a.kind] - order[b.kind];
    }
    if (a.volume !== b.volume) return a.volume.localeCompare(b.volume);
    if (a.path !== b.path) return a.path.localeCompare(b.path);
    return a.name.localeCompare(b.name);
  });
  return entries;
}

/** Tiny async gate: cap in-flight work to `max`, queue the rest FIFO.
 *  Inlined here (rather than importing from lib/limitConcurrency) so
 *  scanLibrary's limiter has its own counter independent of the
 *  per-row meta-fetch limiter — otherwise a big scan would starve the
 *  lazy fetches that happen after render. */
function createScanLimiter(max: number) {
  let active = 0;
  const queue: Array<() => void> = [];
  const next = () => {
    if (active >= max) return;
    const resume = queue.shift();
    if (!resume) return;
    active += 1;
    resume();
  };
  return async function limited<T>(fn: () => Promise<T>): Promise<T> {
    await new Promise<void>((resolve) => {
      queue.push(resolve);
      next();
    });
    try {
      return await fn();
    } finally {
      active -= 1;
      next();
    }
  };
}

/** PS5 game metadata shape — matches the engine's `GameMetaResponse`.
 *  All fields are optional so a folder without param.json still returns
 *  a well-formed object the UI can merge against. */
export interface GameMeta {
  title: string | null;
  title_id: string | null;
  content_id: string | null;
  content_version: string | null;
  application_category_type: number | null;
  has_icon: boolean;
}

/** Base URL of the local engine sidecar. Matches `api/engine.ts::BASE`
 *  and the engine's startup bind address. Duplicated here so the Library
 *  screen can compute icon URLs for `<img src>` without a round-trip to
 *  `commands::engine_url` per row. */
const ENGINE_BASE = "http://127.0.0.1:19113";

/** Fetch title + cover info for a game folder on the PS5. Backing is the
 *  engine's `/api/ps5/game-meta` endpoint which in turn uses FS_READ to
 *  pull `sce_sys/param.json` off the console. Returns a GameMeta with
 *  all fields null + has_icon=false on any failure (path denied, no
 *  param.json, PS5 unreachable) — the UI falls back to the folder name.
 */
export async function fetchGameMeta(
  transferAddr: string,
  path: string
): Promise<GameMeta> {
  const mgmt = toMgmtAddr(transferAddr);
  const url = `${ENGINE_BASE}/api/ps5/game-meta?addr=${encodeURIComponent(
    mgmt
  )}&path=${encodeURIComponent(path)}`;
  const empty: GameMeta = {
    title: null,
    title_id: null,
    content_id: null,
    content_version: null,
    application_category_type: null,
    has_icon: false,
  };
  // 10s is generous for a meta lookup — the engine reads `param.json`
  // (small, hundreds of bytes) and returns. Without the cap, a hung
  // PS5 mid-FS_READ would leave Library's per-row meta-fetch promises
  // pending forever; the row spinner spins until navigation.
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 10_000);
  try {
    const res = await fetch(url, { signal: controller.signal });
    if (!res.ok) return empty;
    const body = (await res.json()) as Partial<GameMeta>;
    return { ...empty, ...body };
  } catch {
    return empty;
  } finally {
    clearTimeout(timer);
  }
}

/** Stable URL pointing at the engine's game-icon endpoint for this folder.
 *  Suitable for `<img src=...>` — the engine streams back `image/png`
 *  (or 404 if no icon). Browser caches the response for 5 minutes. */
export function gameIconUrl(transferAddr: string, path: string): string {
  const mgmt = toMgmtAddr(transferAddr);
  return `${ENGINE_BASE}/api/ps5/game-icon?addr=${encodeURIComponent(
    mgmt
  )}&path=${encodeURIComponent(path)}`;
}

/** Enumerate PS5 storage volumes. Routes through the engine's mgmt-port
 *  FS_LIST_VOLUMES call. `is_placeholder: true` means the OS exposed a
 *  tiny stub (tmpfs) where a drive *could* mount but nothing real is
 *  there — e.g. `/mnt/ext0` when no external is actually attached. UI
 *  should flag those differently from real mounts. */
export async function fetchVolumes(transferAddr: string): Promise<Volume[]> {
  const addr = toMgmtAddr(transferAddr);
  const res = await invoke<{ volumes?: Volume[] }>("ps5_volumes", { addr });
  return Array.isArray(res?.volumes) ? res.volumes : [];
}

/** Swap a PS5 transfer-port addr (`host:9113`) to the management-port
 *  addr (`host:9114`). FS_* frames run on mgmt; the transfer port
 *  rejects them with "wrong_port". Mirrors `mgmt_addr_for` in the engine. */
function toMgmtAddr(transferAddr: string): string {
  const lastColon = transferAddr.lastIndexOf(":");
  if (lastColon < 0) return `${transferAddr}:9114`;
  return `${transferAddr.slice(0, lastColon)}:9114`;
}

/** Probe the destination on the PS5 to decide whether to show the
 *  Override/Resume/Cancel dialog.
 *
 *  - Folder mode (isFolder=true): list `path` and report how many
 *    entries are in it.
 *  - File mode (isFolder=false): list `path`'s PARENT and look for
 *    the target filename — `list_dir` on a file path errors (ENOTDIR),
 *    which would hide an existing file under a "doesn't exist" banner.
 *
 *  Returns `{exists: false}` for real errors too — the caller only
 *  needs to know "something to override?", the specific reason doesn't
 *  affect the decision. */
export async function probeDestination(
  transferAddr: string,
  path: string,
  isFolder: boolean
): Promise<DestinationProbe> {
  const addr = toMgmtAddr(transferAddr);
  const listPath = isFolder ? path : parentDir(path);
  const targetName = isFolder ? null : basenameUnix(path);
  try {
    const res = await invoke<{
      entries?: Array<{ name?: string; size?: number; kind?: string }>;
      truncated?: boolean;
    }>("ps5_list_dir", { addr, path: listPath, offset: 0, limit: 256 });
    const entries = res.entries ?? [];
    if (isFolder) {
      const totalBytes = entries.reduce(
        (acc, e) => acc + (typeof e.size === "number" ? e.size : 0),
        0
      );
      return { exists: true, entryCount: entries.length, totalBytes };
    }
    // File mode: look for our filename in the parent listing.
    const match = entries.find((e) => e.name === targetName);
    if (!match) return { exists: false, entryCount: 0, totalBytes: 0 };
    return {
      exists: true,
      entryCount: 1,
      totalBytes: typeof match.size === "number" ? match.size : 0,
    };
  } catch {
    return { exists: false, entryCount: 0, totalBytes: 0 };
  }
}

function parentDir(absPath: string): string {
  const i = absPath.lastIndexOf("/");
  if (i <= 0) return "/";
  return absPath.slice(0, i);
}

function basenameUnix(absPath: string): string {
  const i = absPath.lastIndexOf("/");
  return i < 0 ? absPath : absPath.slice(i + 1);
}

export type JobStatus = "running" | "done" | "failed";

export interface PlannedFile {
  rel_path: string;
  size: number;
}

export interface JobSnapshot {
  status: JobStatus;
  started_at_ms?: number;
  elapsed_ms?: number;
  /** Populated for `running` (live counter, 200 ms cadence) AND `done`
   *  (final count from COMMIT_TX_ACK). */
  bytes_sent?: number;
  /** Populated for `running` only — total expected bytes so the UI can
   *  render percent + ETA. Pre-computed from source size at job start. */
  total_bytes?: number;
  /** Planned file list for per-file UI. Shipped once on the first
   *  Running tick and republished each tick so polling picks it up
   *  regardless of when the UI subscribes. Empty for tiny jobs. */
  files?: PlannedFile[];
  /** Reconcile-mode skip counts. 0 for plain uploads. */
  skipped_files?: number;
  skipped_bytes?: number;
  /** Files actually sent (Done only). */
  files_sent?: number;
  shards_sent?: number;
  dest?: string;
  error?: string;
}

export async function jobStatus(jobId: string): Promise<JobSnapshot> {
  const raw = await invoke<Record<string, unknown>>("job_status", { jobId });
  // Cheap shape validation. If the Rust side ever returns a non-snapshot
  // payload (e.g., an error envelope we forgot to map to a thrown
  // exception), `status` would be missing and every `=== "done"` /
  // `=== "failed"` branch would silently fall through to the running
  // path — the UI would spin forever on a dead job. Throwing here makes
  // that misconfiguration surface as a visible error.
  const status = typeof raw?.status === "string" ? raw.status : undefined;
  if (status !== "running" && status !== "done" && status !== "failed") {
    throw new Error(
      `job_status returned unexpected shape (missing/invalid status): ${JSON.stringify(raw).slice(0, 200)}`,
    );
  }
  return raw as unknown as JobSnapshot;
}

export interface EngineLogEntry {
  seq: number;
  ts_ms: number;
  level: "info" | "warn" | "error" | string;
  msg: string;
}

export interface EngineLogsTail {
  entries: EngineLogEntry[];
  next_seq: number;
}

/** Pull new engine log lines since the last-seen seq. Response includes
 *  `next_seq` to feed into the next call for incremental polling. */
export async function engineLogsTail(since: number): Promise<EngineLogsTail> {
  const raw = await invoke<Record<string, unknown>>("engine_logs_tail", { since });
  return raw as unknown as EngineLogsTail;
}

/** Locate the `ps5upload.elf` bundled with the app (dev: repo `payload/`,
 *  prod: Tauri Resources). Throws if not built yet. */
export async function bundledPayloadPath(): Promise<string> {
  const resp = await invoke<{ ok: boolean; path?: string; error?: string }>(
    "payload_bundled_path"
  );
  if (!resp.ok || !resp.path) {
    throw new Error(resp.error ?? "payload_bundled_path failed");
  }
  return resp.path;
}

/** Same as `bundledPayloadPath` but returns the file's size + mtime
 *  too so the UI can show a "you're about to send THIS build"
 *  indicator. mtime is seconds since Unix epoch; 0 means unknown. */
export interface BundledPayloadInfo {
  path: string;
  size: number;
  mtime: number;
}
export async function bundledPayloadInfo(): Promise<BundledPayloadInfo> {
  const resp = await invoke<{
    ok: boolean;
    path?: string;
    size?: number;
    mtime?: number;
    error?: string;
  }>("payload_bundled_path");
  if (!resp.ok || !resp.path) {
    throw new Error(resp.error ?? "payload_bundled_path failed");
  }
  return {
    path: resp.path,
    size: resp.size ?? 0,
    mtime: resp.mtime ?? 0,
  };
}

/**
 * Probe a TCP port on the PS5 with a short timeout. Used by the
 * Connection screen to light up the payload / engine status dots
 * without waiting for full HTTP round-trips.
 *
 * Response shape matches the legacy Electron `port_check` — the Rust
 * command returns `{ open: boolean, error?: string }`. This wrapper
 * collapses to just the boolean for callers that only care
 * "did it answer?". For diagnostic contexts that want the error
 * string, call `invoke("port_check", ...)` directly.
 */
export async function portCheck(ip: string, port: number): Promise<boolean> {
  const resp = await invoke<{ open?: boolean; error?: string }>("port_check", {
    ip,
    port,
  });
  return !!resp?.open;
}

/**
 * Check whether the PS5 runtime (:9113) is currently serving. Returns a
 * lightly-parsed shape — the full command's return covers more details
 * but these are the fields the status UI actually renders.
 *
 * When `reachable` is true, `payloadVersion` and `ps5Kernel` come from
 * the STATUS_ACK body (payload fills both fields). They're null when
 * unreachable or when a build predating the `ps5_kernel` field is
 * running — UI shows "—" in that case.
 */
// ─── Self-update ─────────────────────────────────────────────────────
//
// Wraps the Rust-side `update_check` / `update_download` commands in
// src-tauri/src/commands/updates.rs. Flow:
//
//   updateCheck()    → fetches latest.json, returns availability +
//                      release notes + platform-specific download URL
//   updateDownload() → streams the archive into ~/Downloads, opens
//                      the folder in the OS file manager so the user
//                      can finish installing manually
//
// Deliberately no auto-install: the user asked for "notify + download
// + user replaces." See commands/updates.rs for the rationale.

export interface UpdateCheck {
  available: boolean;
  current_version: string;
  latest_version: string;
  notes: string;
  /** ISO-8601 publish timestamp if present in the release manifest,
   *  else empty string. UI should render relative-time off this. */
  pub_date: string;
  /** Non-empty when a bundle exists for the caller's platform. The UI
   *  hides the Download button when this is empty (e.g., a release
   *  that shipped mac-only — linux users see the version bump in
   *  notes but can't install from inside the app). */
  download_url: string;
  /** Last-segment basename of `download_url`; used as the on-disk
   *  filename in ~/Downloads. Pre-split server-side to keep the
   *  renderer free of URL parsing. */
  download_filename: string;
}

export interface UpdateDownload {
  /** Absolute path to the downloaded file on disk. */
  path: string;
  /** Total bytes written. Handy for "Downloaded N MB" status lines. */
  bytes: number;
}

/** Ask GitHub for the latest-release manifest and compare to our
 *  current version. Returns `available:false` when we're current.
 *  Throws on network failure — the UI surfaces that as "Couldn't
 *  check for updates" with the error in a tooltip. */
export async function updateCheck(): Promise<UpdateCheck> {
  return invoke<UpdateCheck>("update_check");
}

/** Stream the update archive into the user's Downloads folder and
 *  reveal it in the OS file manager. Re-running with the same URL
 *  is idempotent: if the file is already on disk with the expected
 *  size, we skip the download and just re-open the folder. */
export async function updateDownload(
  url: string,
  filename: string,
): Promise<UpdateDownload> {
  return invoke<UpdateDownload>("update_download", { url, filename });
}

export async function payloadCheck(
  ip: string
): Promise<{
  reachable: boolean;
  loaded: boolean;
  payloadVersion: string | null;
  ps5Kernel: string | null;
}> {
  const resp = await invoke<{
    reachable?: boolean;
    loaded?: boolean;
    status?: { version?: string; ps5_kernel?: string };
  }>("payload_check", { ip });
  return {
    reachable: !!resp?.reachable,
    loaded: !!resp?.loaded,
    payloadVersion: resp?.status?.version ?? null,
    ps5Kernel: resp?.status?.ps5_kernel ?? null,
  };
}
