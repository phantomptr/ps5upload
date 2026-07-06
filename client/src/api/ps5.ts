// Tauri IPC wrappers — call the Rust-side `#[tauri::command]` handlers
// registered in `client/src-tauri/src/lib.rs`. Prefer this surface over
// `api/engine.ts` for desktop operations: no HTTP roundtrip, no CSP
// allowlisting, and host-local calls (folder inspection, ELF loader
// TCP send) stay in-process.

import { Channel } from "@tauri-apps/api/core";
import { getEngineUrl } from "../state/engine";
// Logging wrapper: every command leaves a trace breadcrumb + logs failures at
// warn. See lib/invokeLogged.ts. (Channel still comes straight from core.)
import { invoke } from "../lib/invokeLogged";
import {
  appendManualListLine,
  SMP_MANUAL_LIST_PATH,
} from "../lib/smpManualList";

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

/** Central-directory preview of a `.zip` game dump. `total_uncompressed` is
 *  what lands on the PS5; `compressed_size` is what the user stores. Game
 *  fields are populated when the archive carries `sce_sys/param.json`. */
export interface ZipInspect {
  file_count: number;
  total_uncompressed: number;
  compressed_size: number;
  title: string | null;
  title_id: string | null;
  content_id: string | null;
  application_category_type: number | null;
  /** Path inside the zip that holds `sce_sys/` (the game root), or null. */
  game_root: string | null;
}

/** Inspect a `.zip` without extracting it. Reads only the central directory
 *  (plus, at most, one small embedded param.json), so it's fast even for a
 *  100 GB archive. Throws if the file isn't a readable zip. */
export async function zipInspect(zipPath: string): Promise<ZipInspect> {
  return invoke<ZipInspect>("zip_inspect", { req: { zip_path: zipPath } });
}

/** A single progress tick from `zipInspectStream`. `entriesSeen` is the
 *  running CDFH-record count while the central directory is parsed —
 *  not the final `file_count` (which excludes directories and sanitized
 *  rejects) but a fine-grained "still moving" signal for the UI. */
export interface ZipInspectProgress {
  entries_seen: number;
}

/** Streaming variant of `zipInspect`. The engine returns SSE — we forward
 *  every `progress` event through `onProgress` so the UI can show a live
 *  entry-count while the central directory is walked, then resolve to the
 *  final `ZipInspect` once the engine emits `event: done`.
 *
 *  Unlike `zipInspect`, this call has no fixed deadline: the Tauri client
 *  uses a **dead-man-switch watchdog** (no SSE chunk for 30 s → fail). On
 *  a healthy engine the keep-alive sends `: heartbeat` every 1 s, so the
 *  watchdog only fires for a genuinely wedged sidecar. This is what lets
 *  the call survive slow cold-cache reads on user-supplied storage
 *  (network mounts, spun-down USB HDDs, 100k+-entry dumps) without
 *  reintroducing the "engine request failed: error sending request"
 *  symptom that the 60 s `http_client()` timeout used to cause.
 *
 *  `onProgress` is fire-and-forget — if the callback throws, subsequent
 *  events still arrive; if it dies silently (e.g. component unmounted),
 *  the final ZipInspect still resolves. */
export async function zipInspectStream(
  zipPath: string,
  onProgress: (p: ZipInspectProgress) => void,
): Promise<ZipInspect> {
  const channel = new Channel<ZipInspectProgress>();
  channel.onmessage = onProgress;
  return invoke<ZipInspect>("zip_inspect_stream", {
    req: { zip_path: zipPath },
    onProgress: channel,
  });
}

// ── .7z (mirror the zip helpers) ──────────────────────────────────────────
// 7z support reuses the ZipInspect / ZipInspectProgress shapes — the engine
// returns the same fields. The host decompresses the archive's single LZMA2
// stream (commonly one .exfat image) forward-only and streams the files into
// the same FTX2 shard pipeline, so they land already-extracted on the PS5.
// Game metadata (title) is not surfaced for 7z: a 7z-of-.exfat has no
// host-visible param.json, so `title` etc. stay null.

/** Inspect a `.7z` without extracting it (file count + sizes). Fast even for a
 *  124 GB archive — only the small header is read. Throws if unreadable. */
export async function sevenzInspect(archivePath: string): Promise<ZipInspect> {
  return invoke<ZipInspect>("sevenz_inspect", {
    req: { archive_path: archivePath },
  });
}

/** Streaming variant of `sevenzInspect` — same dead-man-switch watchdog +
 *  progress channel as `zipInspectStream`. */
export async function sevenzInspectStream(
  archivePath: string,
  onProgress: (p: ZipInspectProgress) => void,
): Promise<ZipInspect> {
  const channel = new Channel<ZipInspectProgress>();
  channel.onmessage = onProgress;
  return invoke<ZipInspect>("sevenz_inspect_stream", {
    req: { archive_path: archivePath },
    onProgress: channel,
  });
}

// ─── Profile (avatar + offline-account username) ──────────────────────────

/** One offline-account name slot. */
export interface ProfileSlot {
  slot: number;
  name: string;
  type: string;
  flags: number;
  id: string;
  activated: boolean;
}

/** A local console user (from /user/home enumeration). */
export interface ProfileUser {
  uid: number;
  uid_hex: string;
  username: string;
}

/** Foreground user + local users + account-name slots. */
export interface ProfileInfo {
  ok: boolean;
  uid: number;
  uid_hex: string;
  username: string;
  users: ProfileUser[];
  slots: ProfileSlot[];
}

export type SquareMode = "crop" | "fit";

/** Read the foreground user, the console's local users, and the
 *  offline-account name slots. */
export async function profileInfo(addr?: string): Promise<ProfileInfo> {
  return invoke<ProfileInfo>("profile_info", { req: { addr: addr ?? null } });
}

/** Rename an offline-account name slot (1-based). */
export async function profileSetUsername(
  slot: number,
  name: string,
  addr?: string,
): Promise<void> {
  await invoke("profile_set_username", {
    req: { addr: addr ?? null, slot, name },
  });
}

/** Rename a local console user (the active profile's display name). */
export async function profileRenameUser(
  uid: number,
  name: string,
  addr?: string,
): Promise<void> {
  await invoke("profile_rename_user", {
    req: { addr: addr ?? null, uid, name },
  });
}

/** Activate an offline-account slot (id derived from the name if omitted). */
export async function profileActivate(
  slot: number,
  id?: number | null,
  addr?: string,
): Promise<{ ok: boolean; id: string }> {
  return invoke("profile_activate", {
    req: { addr: addr ?? null, slot, id: id ?? null },
  });
}

/** De-activate (clear id+flags) an offline-account slot. */
export async function profileClearSlot(
  slot: number,
  addr?: string,
): Promise<void> {
  await invoke("profile_clear_slot", { req: { addr: addr ?? null, slot } });
}

/** Render a 440² crop/fit preview of `imagePath`. Returns a PNG data URL. */
export async function profileAvatarPreview(
  imagePath: string,
  mode: SquareMode,
): Promise<string> {
  const res = await invoke<{ data_url: string }>("profile_avatar_preview", {
    req: { image_path: imagePath, mode },
  });
  return res.data_url;
}

/** Read a user's CURRENT avatar from the PS5 (the squared PNG in Sony's
 *  profile cache), so the picture box can show it by default before a change.
 *  Returns a PNG data URL, or null when the user has no readable custom avatar
 *  (e.g. a stock PSN avatar) — the caller then shows its placeholder. */
export async function profileAvatarCurrent(
  uid: number,
  addr?: string,
): Promise<string | null> {
  const res = await invoke<{ data_url: string | null }>(
    "profile_avatar_current",
    { req: { addr: addr ?? null, uid } },
  );
  return res.data_url;
}

/** Result of an avatar apply: which user it landed on + how many files. */
export interface AvatarApplied {
  uid: number;
  username: string;
  files_copied: number;
}

/** Build + stage + apply a profile avatar from a host image file. Targets
 *  `uid` (0 → resolve the foreground user host-side). */
export async function profileApplyAvatar(
  imagePath: string,
  mode: SquareMode,
  uid?: number | null,
  username?: string | null,
  addr?: string,
): Promise<AvatarApplied> {
  return invoke<AvatarApplied>("profile_apply_avatar", {
    req: {
      addr: addr ?? null,
      image_path: imagePath,
      mode,
      uid: uid ?? null,
      username: username ?? null,
    },
  });
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

/** Start a recursive directory upload. `destRoot` is the final remote
 *  directory whose contents should mirror `srcDir`'s contents. Callers
 *  that want `/data/homebrew/MyGame` must pass that full path, not the
 *  parent `/data/homebrew`. Overwrites whatever is already there — use
 *  `startTransferDirReconcile` for resume. */
export async function startTransferDir(
  srcDir: string,
  destRoot: string,
  addr: string,
  txId?: string | null,
  excludes?: string[],
  bandwidthCapMbps?: number,
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_dir", {
    req: {
      src_dir: srcDir,
      dest_root: destRoot,
      addr,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
      bandwidth_cap_mbps:
        bandwidthCapMbps && bandwidthCapMbps > 0 ? bandwidthCapMbps : null,
    },
  });
  return res.job_id;
}

/** Start a zip-archive upload. The engine decompresses on the host so files
 *  land already extracted under `destRoot` — no temp copy of the whole game,
 *  no payload changes. Same `destRoot` contract as `startTransferDir`: pass
 *  the full final directory (e.g. `/data/homebrew/MyGame`). */
export async function startTransferZip(
  zipPath: string,
  destRoot: string,
  addr: string,
  txId?: string | null,
  excludes?: string[],
  bandwidthCapMbps?: number,
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_zip", {
    req: {
      zip_path: zipPath,
      dest_root: destRoot,
      addr,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
      bandwidth_cap_mbps:
        bandwidthCapMbps && bandwidthCapMbps > 0 ? bandwidthCapMbps : null,
    },
  });
  return res.job_id;
}

/** Start a `.7z`-archive upload. Same contract as `startTransferZip` — the
 *  engine decompresses host-side and the files land already-extracted under
 *  `destRoot`. The common case is a single `.exfat` image. Resume after a
 *  drop re-decompresses from the start (LZMA2 can't seek) and re-sends only
 *  un-acked shards. */
export async function startTransfer7z(
  archivePath: string,
  destRoot: string,
  addr: string,
  txId?: string | null,
  excludes?: string[],
  bandwidthCapMbps?: number,
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_7z", {
    req: {
      archive_path: archivePath,
      dest_root: destRoot,
      addr,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
      bandwidth_cap_mbps:
        bandwidthCapMbps && bandwidthCapMbps > 0 ? bandwidthCapMbps : null,
    },
  });
  return res.job_id;
}

/** Preview a `.rar` (file count + uncompressed size). `password` for encrypted
 *  archives. Throws an Error whose message contains `rar_password_required`
 *  (encrypted headers) or `rar_password_wrong` so the UI can prompt/re-prompt.
 *  Desktop only — see `engineCaps().rar`. */
export async function rarInspect(
  archivePath: string,
  password?: string | null,
): Promise<ZipInspect> {
  return invoke<ZipInspect>("rar_inspect", {
    req: { archive_path: archivePath, password: password ?? null },
  });
}

/** Upload a `.rar`'s contents (any volume set), host-extracted so files land
 *  already-extracted on the PS5. `password` for encrypted archives. Desktop
 *  only. */
export async function startTransferRar(
  archivePath: string,
  destRoot: string,
  addr: string,
  password?: string | null,
  txId?: string | null,
  excludes?: string[],
  bandwidthCapMbps?: number,
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_rar", {
    req: {
      archive_path: archivePath,
      dest_root: destRoot,
      addr,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
      bandwidth_cap_mbps:
        bandwidthCapMbps && bandwidthCapMbps > 0 ? bandwidthCapMbps : null,
      password: password ?? null,
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
  bandwidthCapMbps?: number,
  streams?: number,
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_dir_reconcile", {
    req: {
      src_dir: srcDir,
      dest_root: destRoot,
      addr,
      mode,
      tx_id: txId ?? null,
      excludes: excludes ?? [],
      bandwidth_cap_mbps:
        bandwidthCapMbps && bandwidthCapMbps > 0 ? bandwidthCapMbps : null,
      // Resolved upstream as min(user setting, payload max_transfer_streams).
      // <=1 (or undefined) → single stream, unchanged behaviour.
      streams: streams && streams > 1 ? streams : null,
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

/** Download a PS5 file/folder straight into a `.zip` at `destZip`, streamed
 *  through Deflate as it downloads (no scratch dir). Returns the job_id —
 *  poll job_status for progress, same as a plain download. */
export async function startTransferDownloadZip(
  srcPath: string,
  destZip: string,
  transferAddr: string,
  kind: "file" | "folder",
): Promise<string> {
  const res = await invoke<{ job_id: string }>("transfer_download_zip", {
    req: {
      src_path: srcPath,
      dest_zip: destZip,
      addr: transferAddr,
      kind,
    },
  });
  return res.job_id;
}

// ─── Folder diff (pre-upload preview) ─────────────────────────────────

export interface DirDiffPreview {
  to_send_count: number;
  to_send_bytes: number;
  already_present_count: number;
  already_present_bytes: number;
  /** First 32 relpaths that would be sent. Renderer can show these
   *  as a "preview list" without overwhelming the UI on large diffs. */
  sample_to_send: string[];
}

/** Pre-flight diff: shows what would change without doing the upload.
 *  Always uses Fast (size-only) reconcile mode. */
export async function dirDiffPreview(
  srcDir: string,
  destRoot: string,
  transferAddr: string,
  excludes: string[],
): Promise<DirDiffPreview> {
  return invoke<DirDiffPreview>("transfer_dir_diff_preview", {
    srcDir,
    destRoot,
    addr: transferAddr,
    excludes,
  });
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

/** Enumerate the PS5's mounted storage volumes (internal + extended), with
 *  total/free bytes. Thin wrapper over the `ps5_volumes` command. */
export async function listVolumes(transferAddr: string): Promise<Volume[]> {
  const addr = toMgmtAddr(transferAddr);
  const res = await invoke<{ volumes?: Volume[] }>("ps5_volumes", { addr });
  return res?.volumes ?? [];
}

/** Bytes free across the volumes a game install can land on — the internal
 *  storage plus every extended-storage `/mnt/ext*` mount. Best-effort: returns
 *  null if volumes can't be read (so a check that can't run never blocks the
 *  install).
 *
 *  Internal is reported as `/user` on some consoles and `/data` on others (both
 *  are the one main data partition where `/user/app` lives) — we pick ONE of
 *  them (prefer `/user`) so overlapping mounts aren't double-counted. */
export async function installFreeBytes(
  transferAddr: string,
): Promise<number | null> {
  try {
    const vols = await listVolumes(transferAddr);
    if (vols.length === 0) return null;
    const real = vols.filter((v) => !v.is_placeholder);
    let free = 0;
    let counted = false;
    const internal =
      real.find((v) => v.path === "/user") ??
      real.find((v) => v.path === "/data");
    if (internal) {
      free += Math.max(0, internal.free_bytes);
      counted = true;
    }
    for (const v of real) {
      if (v.path.startsWith("/mnt/ext")) {
        free += Math.max(0, v.free_bytes);
        counted = true;
      }
    }
    return counted ? free : null;
  } catch {
    return null;
  }
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
    let offset = 0;
    while (true) {
      if (signal?.aborted) {
        cancelled = true;
        break;
      }
      if (scanned >= MAX_ENTRIES_SCANNED) {
        truncated = true;
        break;
      }
      let listing: {
        entries?: Array<{ name?: string; kind?: string; size?: number }>;
        truncated?: boolean;
      };
      try {
        listing = await invoke("ps5_list_dir", {
          addr,
          path,
          offset,
          limit: 256,
        });
      } catch {
        break;
      }
      const entries = listing.entries ?? [];
      if (entries.length === 0) break;
      for (const e of entries) {
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
        if (scanned >= MAX_ENTRIES_SCANNED) {
          truncated = true;
          break;
        }
      }
      if (truncated || cancelled || !listing.truncated) break;
      offset += entries.length;
      maybeEmit(path);
    }
    if (truncated || cancelled) {
      break;
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

export async function fsDelete(
  transferAddr: string,
  path: string,
  opId?: number,
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_fs_delete", {
    req: { addr, path, op_id: opId ?? 0 },
  });
}

export interface FsDirEntry {
  name: string;
  kind: "file" | "dir" | "unknown";
  size: number;
}

/** Shallow directory listing. Auto-paginates: the payload caps its
 *  per-frame response at 256 entries regardless of the `limit` we
 *  request (see `payload/src/runtime.c::handle_fs_list_dir`'s
 *  `limit_req > 256` clamp), so callers that need the full listing
 *  of a large dir would otherwise silently see only the first 256.
 *  We loop on the response's `truncated` flag until the payload says
 *  it's exhausted.
 *
 *  Safety cap at 100k entries to bound a runaway/malicious dir. */
export async function fsListDir(
  transferAddr: string,
  path: string,
  _opts?: { offset?: number; limit?: number },
): Promise<FsDirEntry[]> {
  const addr = toMgmtAddr(transferAddr);
  const PAGE = 256; // matches the payload's hard cap
  const HARD_CAP = 100_000;
  const out: FsDirEntry[] = [];
  let offset = 0;
  for (let i = 0; i < HARD_CAP / PAGE; i++) {
    const res = await invoke<{
      entries?: Array<{ name?: string; kind?: string; size?: number }>;
      truncated?: boolean;
    }>("ps5_list_dir", { addr, path, offset, limit: PAGE });
    const page = (res.entries ?? [])
      .filter(
        (e): e is { name: string; kind?: string; size?: number } =>
          typeof e.name === "string" &&
          e.name !== "" &&
          e.name !== "." &&
          e.name !== "..",
      )
      .map((e) => ({
        name: e.name,
        kind: (e.kind === "file" || e.kind === "dir" ? e.kind : "unknown") as
          | "file"
          | "dir"
          | "unknown",
        size: typeof e.size === "number" ? e.size : 0,
      }));
    out.push(...page);
    if (!res.truncated || page.length === 0) break;
    offset += page.length;
    if (out.length >= HARD_CAP) break;
  }
  return out;
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

/** Best-effort "does this path exist on the PS5?" probe. Lists the
 *  parent directory and looks for the basename. There is no FS_STAT
 *  frame in the protocol, so this is the cheapest existence check
 *  we have without inventing a new frame type.
 *
 *  Caveats:
 *  - Returns `false` on listing errors (parent missing, permission
 *    denied, allowlist rejection). Callers must treat `false` as
 *    "best-effort no" rather than a hard guarantee — the payload's
 *    own `fs_copy_dest_exists` check is the real backstop.
 *  - Listing is paginated (limit 1024). For directories with more
 *    entries the basename might be missed; pre-flight callers should
 *    accept that risk and let the payload reject if a race fires.
 *  - Comparison is case-sensitive because the PS5 filesystems we
 *    target (UFS internal, exfat external) are mixed: UFS is
 *    case-sensitive, exfat is case-preserving but case-insensitive.
 *    We err on the side of false-negative (look for an exact match)
 *    so we don't block legitimate moves that differ only in case. */
export async function fsPathExists(
  transferAddr: string,
  path: string,
): Promise<boolean> {
  const norm = path.replace(/\/+$/, "");
  if (norm === "") return true; // root always exists
  const slash = norm.lastIndexOf("/");
  // No slash at all (e.g. "foo" — relative path) shouldn't reach this
  // helper since callers pass PS5 absolute paths, but bail out so we
  // don't accidentally list the wrong directory if it does.
  if (slash < 0) return false;
  const parent = slash === 0 ? "/" : norm.slice(0, slash);
  const basename = norm.slice(slash + 1);
  if (!basename) return false;
  const addr = toMgmtAddr(transferAddr);
  try {
    const res = await invoke<{
      entries?: Array<{ name?: string }>;
    }>("ps5_list_dir", { addr, path: parent, offset: 0, limit: 1024 });
    return (res.entries ?? []).some((e) => e.name === basename);
  } catch {
    return false;
  }
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

/** Result of a successful FS_MOUNT — the payload echoes where it
 *  mounted the image, which dev node it used, and the filesystem it
 *  chose. `read_only` reflects the mode the payload actually applied
 *  (defaults to false for older payloads that don't echo it).
 *  `reused=true` indicates the payload found this image already
 *  mounted and short-circuited; the rest of the result describes the
 *  pre-existing mount. */
export interface MountResult {
  mount_point: string;
  dev_node: string;
  fstype: string;
  read_only?: boolean;
  reused?: boolean;
  /** Image-layout pre-flight result (added 2.2.32). True when
   *  `<mount_point>/sce_sys/param.json` exists immediately after the
   *  mount — meaning Register/Launch will work. False when the image
   *  was built with an extra top-level folder, or when it isn't a
   *  game image. The mount itself succeeds either way; the UI uses
   *  this to surface a warning before the user clicks Register. Older
   *  payloads (<2.2.32) omit the field; engine defaults to true so
   *  pre-2.2.32 mounts don't trigger false warnings. */
  layout_valid?: boolean;
  /** statfs-reported block size at the resolved mount point (2.2.52+).
   *  0 means the field is missing from the payload response (pre-2.2.52)
   *  or the post-mount statfs failed. Useful when diagnosing UFS images
   *  that mount at the kernel level but read as empty — sector-size /
   *  cluster-size mismatches between the .ffpkg image and the LVD
   *  device tend to surface here as an unexpected value. */
  f_bsize?: number;
  /** statfs-reported preferred I/O block size (2.2.52+). Same diagnostic
   *  purpose as `f_bsize` — useful when the kernel reports a fragment
   *  size for `f_bsize` and the actual I/O block size lives here. */
  f_iosize?: number;
  /** True iff the kernel reports MNT_RDONLY on the resolved mount —
   *  even when the caller passed `readOnly=false` (2.2.52+). Sony's
   *  UFS_DOWNLOAD_DATA image_type forces RO on some firmwares and
   *  this flag lets the UI explain why writes won't land. */
  kernel_ro?: boolean;
}

/** Mount a disk image (.exfat / .ffpkg / .ffpfs) on the PS5 via the
 *  payload's built-in LVD-or-MD attach + nmount pipeline.
 *
 *  Mount-location resolution, in priority:
 *    - `mountPoint` (full path) — new in 2.2.25. Mounts at exactly this
 *      path. Must be under a writable root the payload's
 *      `is_path_allowed` accepts (`/data`, `/mnt/ext*`, `/mnt/usb*`,
 *      `/mnt/ps5upload/*`).
 *    - `mountName` (no slashes) — backward-compat. Mounts at
 *      `/mnt/ps5upload/<mountName>/`.
 *    - both omitted — payload derives a filesystem-safe name from
 *      the image basename and mounts at `/mnt/ps5upload/<derived>/`.
 *
 *  `readOnly=true` mounts the image read-only (added in 2.2.26). The
 *  payload selects the matching LVD attach flag and the matching
 *  nmount third-arg flag (UFS magic 0x10000001 for .ffpkg,
 *  MNT_RDONLY for exfatfs and pfs).
 *
 *  Pre-2.2.25 payloads ignore `mountPoint` and only honor `mountName`.
 *  Pre-2.2.26 payloads ignore `readOnly` and always mount RW. */
export async function fsMount(
  transferAddr: string,
  imagePath: string,
  opts?: { mountName?: string; mountPoint?: string; readOnly?: boolean },
): Promise<MountResult> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<MountResult>("ps5_fs_mount", {
    req: {
      addr,
      image_path: imagePath,
      mount_name: opts?.mountName ?? null,
      mount_point: opts?.mountPoint ?? null,
      read_only: opts?.readOnly ?? null,
    },
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

/** Launch a registered title via `sceLncUtilLaunchApp` (re-exposed in
 *  2.2.26). The payload runs a triple-strategy chain — LncUtil with
 *  zeroed param → LncUtil with NULL → SystemServiceLaunchApp fallback —
 *  and surfaces an error containing each return code on failure so a
 *  Library Run-button click that fails has actionable diagnostics
 *  instead of just "didn't launch". The title must already be
 *  registered in the PS5's `app.db`; an unmounted image's title
 *  won't appear there. */
export async function appLaunch(
  transferAddr: string,
  titleId: string,
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_app_launch", { req: { addr, title_id: titleId } });
}

/** What `appRegister` returns after staging + nullfs-binding a game
 *  folder into `/system_ex/app/<title_id>` and calling Sony's
 *  installer. `used_nullfs` is always true today (reserved for
 *  future variants where staging happens without a bind). */
export interface RegisterResult {
  title_id: string;
  title_name: string;
  used_nullfs: boolean;
}

/** Stage + register a game folder so the PS5 XMB picks it up
 *  (re-exposed in 2.2.26). `srcPath` must be a directory containing
 *  `sce_sys/param.json` or `sce_sys/param.sfo`. Works for folders
 *  on `/data`, `/mnt/ext*`, `/mnt/usb*`, AND content inside a mounted
 *  `/mnt/ps5upload/<name>/`. Idempotent — calling twice on the same
 *  source path is a no-op.
 *
 *  `opts.patchDrmType=true` (added in 2.2.26) rewrites the source
 *  `sce_sys/param.json`'s `applicationDrmType` to `"standard"`
 *  before staging — needed when a PSN-extracted dump ships with
 *  `"PSN"` or `"disc"` and the PS5 launcher rejects it with a DRM
 *  error. **Modifies the user's source file in place** (no temp
 *  + rename — most PS5 mounted-image filesystems reject cross-FS
 *  rename), so it's opt-in. The patch is no-op when the existing
 *  value is already `"standard"`. */
export async function appRegister(
  transferAddr: string,
  srcPath: string,
  opts?: { patchDrmType?: boolean },
): Promise<RegisterResult> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<RegisterResult>("ps5_app_register", {
    req: {
      addr,
      src_path: srcPath,
      patch_drm_type: opts?.patchDrmType ?? null,
    },
  });
}

/** Reverse of `appRegister`: tears down the nullfs at
 *  `/system_ex/app/<titleId>`, removes tracking, calls Sony's
 *  AppUninstall when available. Best-effort — succeeds even when
 *  Sony's API isn't available on the running firmware as long as
 *  the unmount went through. */
export async function appUnregister(
  transferAddr: string,
  titleId: string,
): Promise<void> {
  const addr = toMgmtAddr(transferAddr);
  await invoke("ps5_app_unregister", {
    req: { addr, title_id: titleId },
  });
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
  /** Sony's packed firmware version word read via kernel R/W. 0
   *  when unavailable on the payload's current loader (no kstuff
   *  → no kernel R/W). When non-zero this is more precise than the
   *  parsed kern.version string — distinguishes FW points within
   *  a family (9.60 vs 9.6010). Added 2.13.0 via the
   *  ps5-payload-sdk 2026-05 update. */
  kernel_fw_version: number;
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
  /** Average CPU usage across cores, 0–100. `-1` = unavailable (basic
   *  read, or the API isn't exported on this firmware). Populated only by
   *  an extended ("Read sensors") read. */
  cpu_usage_pct: number;
  /** Current fan duty, 0–100 (approximate). `-1` = unavailable. Extended
   *  read only. */
  fan_duty_pct: number;
  /** Raw Sony "basic product shape" code (distinguishes hardware
   *  families). `-1` = unavailable. The model string stays the primary
   *  identifier; this is a cross-check. Extended read only. */
  product_shape: number;
}

export interface HwPower {
  operating_time_sec: number;
  operating_time_hours: number;
  operating_time_minutes: number;
  boot_count: number;
  power_consumption_mw: number;
  /** System load average (1, 5, 15 minute windows). Same shape as
   *  BSD/Linux: 0.00 = idle, 1.00 per logical core = fully loaded.
   *  Negative value means the kernel didn't return a reading on
   *  this firmware (treat as "unavailable" in the UI). Added 2.13.0
   *  via the SDK's `getloadavg()` from the 2026-05 update. */
  load_avg_1m: number;
  load_avg_5m: number;
  load_avg_15m: number;
}

export async function fetchHwInfo(transferAddr: string): Promise<HwInfo> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwInfo>("ps5_hw_info", { addr });
}

/** Read live sensors. `extended` (default false) selects the on-demand
 *  telemetry — SoC power, CPU usage, fan duty, product shape. Pass `true`
 *  only from an explicit user action ("Read sensors"); the Dashboard's
 *  auto-poll must call this WITHOUT extended so its 5s tick only ever
 *  triggers the basic, always-safe read on the payload. */
export async function fetchHwTemps(
  transferAddr: string,
  extended = false,
): Promise<HwTemps> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwTemps>("ps5_hw_temps", { addr, extended });
}

export async function fetchHwPower(transferAddr: string): Promise<HwPower> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwPower>("ps5_hw_power", { addr });
}

/** "Console Storage" aggregate matching what PS5 Settings shows
 *  (added in 2.2.26). Sums `/user effective + /system_data + /system_ex`
 *  with reserved-space accounting. Distinct from `fsListVolumes` —
 *  that returns per-volume detail; this is the single-line summary. */
export interface HwStorage {
  total_bytes: number;
  free_bytes: number;
  used_bytes: number;
  reserved_bytes: number;
  user_total_bytes: number;
  user_free_bytes: number;
  user_reserved_bytes: number;
  system_data_total_bytes: number;
  system_data_free_bytes: number;
  system_ex_total_bytes: number;
  system_ex_free_bytes: number;
}

export async function fetchHwStorage(transferAddr: string): Promise<HwStorage> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<HwStorage>("ps5_hw_storage", { addr });
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

// ─── LAN discovery (mDNS-SD + TCP probe) ──────────────────────────────
// Backed by `discover_ps5` in the Rust command layer. Single one-shot
// browse — the renderer triggers it from the Connection screen's "Find
// PS5s" button. Returns candidates sorted by confidence so the UI can
// render them without re-sorting; the first row is the best guess.

export interface DiscoveredHost {
  ip: string;
  hostname: string | null;
  /** All IPv4/IPv6 addresses we observed for this host. The `ip` field
   *  is the best one to type into the host input; `all_ips` is shown
   *  as a tooltip / "Use alternate" affordance for dual-homed PS5s. */
  all_ips: string[];
  /** mDNS service types observed (without the trailing `.local`). */
  services: string[];
  /** :9021 reachable — the universal payload-loader port. */
  loader_port_open: boolean;
  /** :9114 reachable — our own payload is already running here. */
  payload_port_open: boolean;
  /** 0-100 score; see commands/discover.rs::confidence for weights. */
  confidence: number;
  /** Pre-computed `confidence >= 50` so the renderer doesn't repeat
   *  the threshold value. UI shows a green badge when true. */
  likely_ps5: boolean;
}

export interface DiscoverResult {
  ok: boolean;
  candidates: DiscoveredHost[];
  scanned_ms: number;
  browsed_services: string[];
  /** Present only on init failure (e.g., mDNS daemon refused to start
   *  because something else holds the port). UI shows it inline. */
  error?: string;
}

/** Browse the LAN for PS5 candidates via mDNS-SD + TCP probe.
 *  `timeoutSecs` is clamped to [1, 10] on the Rust side; default 3s. */
export async function discoverPs5(
  timeoutSecs?: number,
): Promise<DiscoverResult> {
  return invoke<DiscoverResult>("discover_ps5", {
    timeoutSecs: timeoutSecs ?? null,
  });
}

// ─── Payload Library (curated catalogue + GitHub fetch) ───────────────
// Backed by `commands/payloads.rs`. Three primitives:
//   - catalogue: static list of supported payloads
//   - release: fetch latest GitHub release (cached on disk)
//   - inventory + download: manage the on-disk cache the Send and
//     USB-autoloader flows both consume

export interface PayloadInfo {
  id: string;
  display_name: string;
  role: string;
  description: string;
  repo_owner: string;
  repo_name: string;
  on_console_marker_path: string | null;
  process_name_hint: string | null;
  ports: number[];
  autoload_priority: number;
  autoload_delay_ms: number;
  homepage: string;
}

export interface PayloadReleaseInfo {
  payload_id: string;
  tag: string;
  name: string;
  body: string;
  published_at: string;
  html_url: string;
  picked_asset_url: string;
  picked_asset_name: string;
  picked_asset_size: number;
  /** True when GitHub/Gitea marked this release a pre-release. The UI
   *  badges it "pre-release" so a user picking a version knows it may
   *  be unstable (payload dev moves fast). */
  prerelease?: boolean;
  cached_age_secs: number;
  /** Present when the response came from cache because the live
   *  fetch failed (network down, GitHub rate-limited, 5xx outage,
   *  malformed body). UI should render a banner so the user knows
   *  the data is potentially stale and what went wrong, instead of
   *  silently presenting "old data as if fresh". Omitted (undefined)
   *  when the fetch succeeded. Mirrors sonicloader's `refreshError`
   *  field — see backend `payloads.rs::ReleaseInfo`. */
  refresh_error?: string;
}

export interface PayloadLocalEntry {
  payload_id: string;
  version: string;
  path: string;
  size: number;
  mtime: number;
}

export async function payloadsCatalog(): Promise<PayloadInfo[]> {
  return invoke<PayloadInfo[]>("payloads_catalog");
}

export async function payloadsRelease(
  id: string,
  forceRefresh = false,
): Promise<PayloadReleaseInfo> {
  return invoke<PayloadReleaseInfo>("payloads_release", {
    id,
    forceRefresh,
  });
}

/** List ALL releases for a catalogue payload (newest first) so the user
 *  can pick a specific version — pin a known-good build or downgrade when
 *  the latest is unstable. Each entry is a full `PayloadReleaseInfo`
 *  (same shape as `payloadsRelease`), so the chosen one's
 *  `picked_asset_url` + `tag` feed straight into `payloadsDownload`. */
export async function payloadsReleases(
  id: string,
  forceRefresh = false,
): Promise<PayloadReleaseInfo[]> {
  return invoke<PayloadReleaseInfo[]>("payloads_releases", {
    id,
    forceRefresh,
  });
}

export async function payloadsLocalInventory(): Promise<PayloadLocalEntry[]> {
  return invoke<PayloadLocalEntry[]>("payloads_local_inventory");
}

export async function payloadsLocalPath(id: string): Promise<string | null> {
  return invoke<string | null>("payloads_local_path", { id });
}

export async function payloadsDownload(
  id: string,
  assetUrl: string,
  version: string,
): Promise<PayloadLocalEntry> {
  return invoke<PayloadLocalEntry>("payloads_download", {
    id,
    assetUrl,
    version,
  });
}

// ─── ShadowMount+ awareness (read-only) ───────────────────────────────

export interface SmpMountedImage {
  mount_point: string;
  derived_name: string;
}

export interface SmpStatus {
  installed: boolean;
  running: boolean;
  config_ini: string | null;
  autotune_ini: string | null;
  debug_log_tail: string | null;
  mounted_images: SmpMountedImage[];
  errors: string[];
}

/** SMP status snapshot. `addr` is the management-port address. */
export async function smpStatus(addr: string): Promise<SmpStatus> {
  return invoke<SmpStatus>("smp_status", { addr });
}

// ─── USB autoloader wizard ────────────────────────────────────────────

export interface UsbDrive {
  path: string;
  label: string;
  free_bytes: number;
  total_bytes: number;
}

export interface AutoloaderInstallResult {
  written: string[];
  autoload_txt: string;
  skipped: string[];
}

export async function usbListRemovable(): Promise<UsbDrive[]> {
  return invoke<UsbDrive[]>("usb_list_removable");
}

export async function usbAutoloaderInstall(req: {
  drive_path: string;
  payload_ids: string[];
  include_ps5upload?: boolean;
}): Promise<AutoloaderInstallResult> {
  return invoke<AutoloaderInstallResult>("usb_autoloader_install", { req });
}

// ─── Game metadata healing ────────────────────────────────────────────

export interface HealOutcome {
  file: string;
  status:
    | "copied"
    | "already_present"
    | "missing_from_source"
    | "skipped_no_source"
    | "error";
  error: string | null;
}

export interface HealResult {
  title_id: string;
  appmeta_dir: string;
  source_dir: string;
  outcomes: HealOutcome[];
  copied: number;
  already_present: number;
  errors: number;
}

/** Restore missing /user/appmeta/<TID>/ files (icon0.png, param.json,
 *  snd0.at9) from the game source's sce_sys/. Idempotent — files
 *  already in /user/appmeta/<TID>/ are left untouched. `addr` is the
 *  management-port address. */
export async function healAppmeta(
  addr: string,
  titleId: string,
  sourcePath: string,
): Promise<HealResult> {
  return invoke<HealResult>("heal_appmeta", {
    addr,
    titleId,
    sourcePath,
  });
}

// ─── Local .ffpkg inspector (UFS2 reader) ─────────────────────────────

export interface FfpkgRootEntry {
  name: string;
  kind: "dir" | "file" | "link" | "other";
  size: number;
}

export interface FfpkgInspection {
  /** Total bytes per the UFS2 superblock (block_count × block_size). */
  image_bytes: number;
  block_size: number;
  fragment_size: number;
  /** Volume label from the superblock (usually empty for PS5 .ffpkg). */
  volume_name: string;
  /** True when sce_sys/ folder is present at root. */
  has_sce_sys: boolean;
  /** PARAM.SFO TITLE_ID (e.g. "CUSA12345") if found. */
  title_id: string | null;
  /** PARAM.SFO TITLE (game name). */
  title: string | null;
  /** PARAM.SFO CATEGORY ("gd"=game, "gp"=patch, "gc"=add-on, ...). */
  category: string | null;
  root_entries: FfpkgRootEntry[];
  warnings: string[];
}

/** Inspect a UFS2 image (.ffpkg / .ufs) locally — no PS5 needed.
 *  Reads the superblock, lists root entries, parses sce_sys/param.sfo
 *  if present. Cheap (~100ms typical) regardless of image size. */
export async function ffpkgInspect(path: string): Promise<FfpkgInspection> {
  return invoke<FfpkgInspection>("ffpkg_inspect", { path });
}

export interface FfpkgExtractResult {
  file_count: number;
  bytes_written: number;
  /** First 32 paths written. Useful for renderer "wrote N files" with
   *  a "show files" expander. */
  sample_paths: string[];
}

/** Extract a file or subtree from a local .ffpkg to a local dir.
 *  `innerPath` is slash-separated; empty string means the whole image
 *  root. Read-only on the image. */
export async function ffpkgExtract(
  ffpkgPath: string,
  innerPath: string,
  destDir: string,
): Promise<FfpkgExtractResult> {
  return invoke<FfpkgExtractResult>("ffpkg_extract", {
    ffpkgPath,
    innerPath,
    destDir,
  });
}

// ─── PS5 power control (reboot/shutdown/standby/tick) ─────────────────

export interface PowerControlAck {
  ok: boolean;
  action?: string;
  err?: string;
  code?: number;
}

/** Reboot the PS5. Async — the connection drops as soon as Sony's
 *  reboot starts, but the payload sends ACK first; core treats the
 *  expected drop as success. */
export async function powerReboot(addr: string): Promise<PowerControlAck> {
  return invoke<PowerControlAck>("power_reboot", { addr });
}

/** Power off the PS5. Same drop-is-success semantics as reboot. */
export async function powerShutdown(addr: string): Promise<PowerControlAck> {
  return invoke<PowerControlAck>("power_shutdown", { addr });
}

/** One row in the process manager. `kind` drives the UI's filter + kill
 *  guard: "app" (user game/app), "payload" (user .elf homebrew), or
 *  "system" (Sce* daemons + Sony NPXS apps — hidden by default, killing
 *  them can crash the console). */
export interface ProcessInfo {
  pid: number;
  /** Thread name (ki_tdname). */
  name: string;
  /** Command/executable name (ki_comm), e.g. "eboot.bin". */
  comm: string;
  /** Title id for app/game processes; empty otherwise. */
  title_id: string;
  app_id: number;
  /** Resident set size in MiB. */
  memory_mib: number;
  threads: number;
  kind: "app" | "payload" | "system";
  /** True for the PS5Upload helper's own process — it can't be killed (doing so
   *  would sever the tool's connection), so the UI disables Kill/Restart on it. */
  is_self?: boolean;
}

export interface ProcessListResult {
  processes: ProcessInfo[];
  /** True when the payload truncated the list to fit its buffer. */
  truncated: boolean;
}

export interface ProcessKillAck {
  ok: boolean;
  pid: number;
  err?: string | null;
}

/** Enumerate running processes (detailed). `addr` is the mgmt addr
 *  (ip:9114). Read-only. */
export async function processList(addr: string): Promise<ProcessListResult> {
  return invoke<ProcessListResult>("process_list_get", { addr });
}

/** SIGKILL a process by pid. The payload guards self/kernel/init; the
 *  caller must confirm before killing a "system" process. */
export async function processKill(
  addr: string,
  pid: number,
): Promise<ProcessKillAck> {
  return invoke<ProcessKillAck>("process_kill_pid", { addr, pid });
}

/** Enter rest mode (standby). May be unavailable on some firmware
 *  revisions where the symbol moved — ack carries `err:"standby_unavailable"`
 *  in that case. */
export async function powerStandby(addr: string): Promise<PowerControlAck> {
  return invoke<PowerControlAck>("power_standby", { addr });
}

/** Defer the auto-sleep timer by one tick. Non-destructive; useful
 *  to run on a schedule during long uploads to keep the PS5 awake. */
export async function powerTick(addr: string): Promise<PowerControlAck> {
  return invoke<PowerControlAck>("power_tick", { addr });
}

export interface PowerTelemetry {
  /** Cumulative power-on seconds since first boot. Null when this PS5
   *  generation doesn't expose the metric. */
  operating_seconds: number | null;
  /** Boot/shutdown cycle count. */
  boot_cycles: number | null;
  /** Bit-flagged thermal alert state. 0 = no alert. */
  thermal_alert_flags: number | null;
  /** Power-up cause code (Sony-internal codes — surfaced raw). */
  power_up_cause: number | null;
}

/** Fetch lifetime ICC telemetry (operating seconds, boot count,
 *  thermal alerts). Read-only — safe even on payloads without
 *  kernel R/W. */
export async function powerTelemetryGet(addr: string): Promise<PowerTelemetry> {
  return invoke<PowerTelemetry>("power_telemetry_get", { addr });
}

// ─── User account enumeration ─────────────────────────────────────────

export interface UserAccount {
  id: number;
  name: string;
  /** True for the currently-active foreground user. */
  foreground: boolean;
  /** Sony API error code from sceUserServiceGetUserName. 0 = ok. */
  err_name: number;
}

export interface UserList {
  /** User id of the foreground user, or -1 when unavailable. */
  foreground: number;
  err_fg: number;
  err_list: number;
  users: UserAccount[];
}

/** Enumerate logged-in user accounts on the PS5. */
export async function userListGet(addr: string): Promise<UserList> {
  return invoke<UserList>("user_list_get", { addr });
}

// ─── Save data + screenshot listing ───────────────────────────────────

export interface SaveEntry {
  title_id: string;
  user_id: number;
  path: string;
  size: number;
  mtime: number;
  /** "ps5" for native, "ps4" for legacy savedata. */
  kind: "ps5" | "ps4";
}

export interface SaveList {
  saves: SaveEntry[];
}

/** List save data folders. user_id=0 lists every user's saves. */
export async function savesList(
  addr: string,
  userId?: number,
): Promise<SaveList> {
  return invoke<SaveList>("saves_list", {
    addr,
    userId: userId ?? null,
  });
}

export interface ScreenshotEntry {
  path: string;
  size: number;
  mtime: number;
}

export interface ScreenshotList {
  items: ScreenshotEntry[];
}

/** List screenshots from the PS5 (full-res `/user/av_contents/photo` plus
 *  thumbnails, deduped so each shot appears once). */
export async function screenshotsList(addr: string): Promise<ScreenshotList> {
  return invoke<ScreenshotList>("screenshots_list", { addr });
}

/** List gameplay video clips from the PS5 (`/user/av_contents/video`,
 *  `.webm`/`.mp4`). Same `{path,size,mtime}` shape as screenshots so the
 *  Videos screen reuses the row + generic download path. Video clips
 *  don't need the `.jxr → PNG` conversion screenshots do — they download
 *  as-is and play in any modern player. */
export async function videosList(addr: string): Promise<ScreenshotList> {
  return invoke<ScreenshotList>("videos_list", { addr });
}

/** Convert a downloaded PS5 screenshot (`.jxr` / JPEG XR — HDR, not
 *  viewable in normal apps) into an SDR PNG. Decoding + HDR→SDR
 *  tone-mapping happen in-process in the Rust backend (jxrlib, no external
 *  tool). `srcPath`/`dstPath` are local host paths; `deleteSource` removes
 *  the intermediate `.jxr` after a successful conversion. Returns the PNG
 *  path. Desktop only — errors on mobile. */
export async function convertScreenshot(
  srcPath: string,
  dstPath: string,
  deleteSource: boolean,
): Promise<string> {
  return invoke<string>("screenshot_convert", {
    srcPath,
    dstPath,
    deleteSource,
  });
}

// ─── Save data .zip backup / restore ──────────────────────────────────
//
// The Saves screen wraps backups into `<title_id>.zip` (containing
// `<title_id>/<files>` at the zip root). These four commands are thin
// helpers around the local zip envelope — the actual PS5 ↔ host bytes
// still travel through `transfer_download` / `transfer_dir`.

/** Create a scratch directory under the OS temp root. The returned
 *  absolute path is what the caller passes as `destDir` to
 *  `startTransferDownload` (for backup) or as `destDir` to
 *  `saveArchiveUnzip` (for restore). Always pair with
 *  `saveArchiveCleanupTemp` in a `finally` block. */
export async function saveArchiveMakeTemp(prefix: string): Promise<string> {
  return invoke<string>("save_archive_make_temp", { prefix });
}

/** Best-effort recursive delete of a scratch dir. Refuses to remove
 *  anything outside the system temp root, so a bad call can't wipe
 *  user data. Returns immediately on missing paths. */
export async function saveArchiveCleanupTemp(path: string): Promise<void> {
  await invoke("save_archive_cleanup_temp", { path });
}

/** Zip `<srcDir>/<innerRoot>/` into `destZip`. The resulting archive's
 *  top-level entry is `<innerRoot>/` — i.e. the canonical
 *  `<title_id>.zip` ⇒ `<title_id>/<files>` shape. */
export async function saveArchiveZip(
  srcDir: string,
  innerRoot: string,
  destZip: string,
  // The filename offered as the dialog's defaultPath. On Android `destZip`
  // is a content:// URI with no usable name; pass this so the backend keeps
  // distinct backups from colliding on one fixed Downloads filename.
  fileName?: string,
): Promise<void> {
  await invoke("save_archive_zip", {
    req: {
      src_dir: srcDir,
      inner_root: innerRoot,
      dest_zip: destZip,
      dest_filename: fileName,
    },
  });
}

export interface SaveArchiveUnzipResult {
  inner_root: string;
  file_count: number;
}

/** Strict-validate + extract `zipPath` into `destDir`. The zip must
 *  contain exactly one top-level folder named `expectedInner`; any
 *  other layout (flat files at root, multiple roots, wrong name) is
 *  rejected before any bytes are written.
 *
 *  After success, the extracted save lives at
 *  `<destDir>/<expectedInner>/…` and can be uploaded with
 *  `startTransferDir`. */
export async function saveArchiveUnzip(
  zipPath: string,
  destDir: string,
  expectedInner: string,
): Promise<SaveArchiveUnzipResult> {
  return invoke<SaveArchiveUnzipResult>("save_archive_unzip", {
    req: {
      zip_path: zipPath,
      dest_dir: destDir,
      expected_inner: expectedInner,
    },
  });
}

export interface SaveArchiveBackupFinalizeResult {
  kept_files: number;
  stripped_count: number;
  dropped_dirs: number;
}

/** Post-download cleanup before zipping. Strips Sony's `sdimg_` prefix
 *  from PS4-format images (byte 0 = 0x01) so backups look like the
 *  cross-tool resigner convention. Drops any subdirectory of the title
 *  folder other than `sce_sys/` — that's where Sony stores nested
 *  emulator bookkeeping for PS2-Classics, and it shouldn't pollute the
 *  user-facing backup zip. PS5-native images (byte 0 = 0x02) and
 *  `.bin` sealed keys are left alone. */
export async function saveArchiveBackupFinalize(
  srcDir: string,
  titleId: string,
): Promise<SaveArchiveBackupFinalizeResult> {
  return invoke<SaveArchiveBackupFinalizeResult>(
    "save_archive_backup_finalize",
    { req: { src_dir: srcDir, title_id: titleId } },
  );
}

export interface SaveArchiveRestorePrepareResult {
  renamed_count: number;
}

/** Pre-upload preparation after unzipping. Re-adds `sdimg_` prefix to
 *  bare top-level image files (those whose prefix was stripped during
 *  backup) so the PS5 sees the filename it expects. Files that already
 *  start with `sdimg_`, end in `.bin`, or live under `sce_sys/` pass
 *  through unchanged. */
export async function saveArchiveRestorePrepare(
  srcDir: string,
  titleId: string,
): Promise<SaveArchiveRestorePrepareResult> {
  return invoke<SaveArchiveRestorePrepareResult>(
    "save_archive_restore_prepare",
    { req: { src_dir: srcDir, title_id: titleId } },
  );
}

// ─── Filesystem search index ──────────────────────────────────────────

export interface IndexStartResult {
  started: boolean;
  err?: string;
}

export interface IndexStatusResult {
  phase: "idle" | "building" | "ready";
  files: number;
  started_at: number;
  completed_at: number;
}

export interface SearchIndexHit {
  path: string;
  size: number;
}

export interface SearchResults {
  results: SearchIndexHit[];
}

export async function fsIndexStart(
  addr: string,
  roots?: string[],
): Promise<IndexStartResult> {
  return invoke<IndexStartResult>("fs_index_start", {
    addr,
    roots: roots ?? null,
  });
}

export async function fsIndexStatus(addr: string): Promise<IndexStatusResult> {
  return invoke<IndexStatusResult>("fs_index_status", { addr });
}

export async function fsSearchIndex(
  addr: string,
  query: string,
  opts?: { sizeMin?: number; sizeMax?: number; limit?: number },
): Promise<SearchResults> {
  return invoke<SearchResults>("fs_search_index", {
    addr,
    query,
    sizeMin: opts?.sizeMin ?? null,
    sizeMax: opts?.sizeMax ?? null,
    limit: opts?.limit ?? null,
  });
}

export async function fsIndexCancel(addr: string): Promise<void> {
  return invoke<void>("fs_index_cancel", { addr });
}

// ─── App lifecycle + rich toast ───────────────────────────────────────

export interface RunningApp {
  app_id: number;
}

export interface AppLifecycleAck {
  ok: boolean;
  action?: string;
  app_id?: number;
  code?: number;
  err?: string;
  apps?: RunningApp[];
}

export async function appSuspend(addr: string, appId: number): Promise<AppLifecycleAck> {
  return invoke<AppLifecycleAck>("app_suspend", { addr, appId });
}
export async function appResume(addr: string, appId: number): Promise<AppLifecycleAck> {
  return invoke<AppLifecycleAck>("app_resume", { addr, appId });
}
export async function appKill(addr: string, appId: number): Promise<AppLifecycleAck> {
  return invoke<AppLifecycleAck>("app_kill", { addr, appId });
}
export async function appListRunning(addr: string): Promise<AppLifecycleAck> {
  return invoke<AppLifecycleAck>("app_list_running", { addr });
}

export interface ToastPushAck {
  ok: boolean;
  code?: number;
  err?: string;
}

/** Push a styled toast notification to the PS5. Best-effort —
 *  malformed templates are silently dropped by Sony's daemon. */
export async function toastPush(
  addr: string,
  title: string,
  opts?: { subtitle?: string; icon?: string; actionUrl?: string },
): Promise<ToastPushAck> {
  return invoke<ToastPushAck>("toast_push", {
    addr,
    title,
    subtitle: opts?.subtitle ?? null,
    icon: opts?.icon ?? null,
    actionUrl: opts?.actionUrl ?? null,
  });
}

// ─── Diagnostics RPCs ─────────────────────────────────────────────────

/** Drain the kernel log buffer into a string. Empty when nothing
 *  new arrived since the last call. */
export async function klogChunk(addr: string, maxBytes?: number): Promise<string> {
  return invoke<string>("klog_chunk", {
    addr,
    maxBytes: maxBytes ?? null,
  });
}

export interface NetInterface {
  name: string;
  mac: string;
  ipv4: string;
  mtu: number;
  flags: number;
}
export interface NetInterfaceList {
  interfaces: NetInterface[];
  err?: string;
}
export async function netInterfacesGet(addr: string): Promise<NetInterfaceList> {
  return invoke<NetInterfaceList>("net_interfaces_get", { addr });
}

export interface PeripheralAck {
  ok: boolean;
  action?: string;
  port?: number;
  code?: number;
  err?: string;
}
export async function peripheralEject(addr: string): Promise<PeripheralAck> {
  return invoke<PeripheralAck>("peripheral_eject", { addr });
}
export async function peripheralBdOff(addr: string): Promise<PeripheralAck> {
  return invoke<PeripheralAck>("peripheral_bd_off", { addr });
}
export async function peripheralBdOn(addr: string): Promise<PeripheralAck> {
  return invoke<PeripheralAck>("peripheral_bd_on", { addr });
}
export async function peripheralUsbOff(addr: string, port: number): Promise<PeripheralAck> {
  return invoke<PeripheralAck>("peripheral_usb_off", { addr, port });
}
export async function peripheralUsbOn(addr: string, port: number): Promise<PeripheralAck> {
  return invoke<PeripheralAck>("peripheral_usb_on", { addr, port });
}

export interface ModuleInfo {
  handle: number;
  name: string;
  /** Hex string ("0x7f1234…") — preserved as string to avoid JS
   *  Number precision loss past 2^53. */
  base: string;
  code_size: number;
}
export interface ModuleList {
  modules: ModuleInfo[];
  err?: string;
}
export async function procModulesGet(addr: string, pid?: number): Promise<ModuleList> {
  return invoke<ModuleList>("proc_modules_get", {
    addr,
    pid: pid ?? null,
  });
}

export interface ProcEntry {
  pid: number;
  name: string;
}
export interface ProcList {
  ok: boolean;
  procs: ProcEntry[];
  truncated?: boolean;
  error?: string;
}
/** Full running-process list (pid + name) — the rich "what's running" used by
 *  the bug-report snapshot. Distinct from {@link procModulesGet}. */
export async function procListGet(addr: string): Promise<ProcList> {
  return invoke<ProcList>("proc_list_get", { addr });
}

// ─── Shell + CRC32 + app.db + speed test ──────────────────────────────

export interface ShellRunResult {
  exit_code?: number;
  timed_out: boolean;
  stdout: string;
  cwd?: string;
  session_id?: string;
  err?: string;
}

/** Run a shell command on the PS5. timeout_secs caps payload-side
 *  wait. stdout (with stderr merged) capped at 256 KB. */
export async function shellRun(
  addr: string,
  cmd: string,
  sessionId?: string,
  cwd?: string,
  timeoutSecs?: number,
): Promise<ShellRunResult> {
  return invoke<ShellRunResult>("shell_run_cmd", {
    addr,
    cmd,
    sessionId: sessionId ?? null,
    cwd: cwd ?? null,
    timeoutSecs: timeoutSecs ?? null,
  });
}

export interface Crc32Result {
  crc32?: number;
  size?: number;
  err?: string;
}

export async function crc32File(addr: string, path: string): Promise<Crc32Result> {
  return invoke<Crc32Result>("crc32_file_get", { addr, path });
}

export interface AppDbEntry {
  title_id: string;
  app_id: number;
  name: string;
}

export interface AppDbList {
  apps: AppDbEntry[];
  err?: string;
}

export async function appdbQuery(addr: string): Promise<AppDbList> {
  return invoke<AppDbList>("appdb_query_get", { addr });
}

export interface NetSpeedTestResult {
  round_trips: number;
  elapsed_ms: number;
  avg_rtt_us: number;
  p50_rtt_us: number;
  p95_rtt_us: number;
}

export async function netSpeedTestRun(
  addr: string,
  roundTrips?: number,
): Promise<NetSpeedTestResult> {
  return invoke<NetSpeedTestResult>("net_speed_test_run", {
    addr,
    roundTrips: roundTrips ?? null,
  });
}

export interface PkgDirectMountResult {
  ok: boolean;
  code?: number;
  mount_point?: string;
  err?: string;
}

/** Mount a .pkg file directly via sceFsMountGamePkg (bypasses BGFT
 *  install). Faster for testing patches; doesn't register the title
 *  in the home screen. */
export async function pkgDirectMount(
  addr: string,
  pkgPath: string,
  mountPoint?: string,
): Promise<PkgDirectMountResult> {
  return invoke<PkgDirectMountResult>("pkg_direct_mount_run", {
    addr,
    pkgPath,
    mountPoint: mountPoint ?? null,
  });
}

export interface UfsFsckResult {
  ok: boolean;
  code?: number;
  device?: string;
  repair: boolean;
  err?: string;
}

/** Run UFS fsck on a device. repair=false is a read-only check;
 *  true attempts repair (UI should confirm before sending true). */
export async function ufsFsck(
  addr: string,
  device: string,
  repair: boolean,
): Promise<UfsFsckResult> {
  return invoke<UfsFsckResult>("ufs_fsck_run", { addr, device, repair });
}

export interface FsReadPreviewResult {
  size: number;
  /** Base64-encoded file bytes. Use as `data:image/...;base64,<…>`
   *  for images, or decode to UTF-8 for text. Capped at 256 KB. */
  base64: string;
}

export interface LwfsMountResult {
  ok: boolean;
  code?: number;
  mount_point?: string;
  title_id?: string;
  err?: string;
}

export interface FsWriteBytesResult {
  ok: boolean;
  size?: number;
  err?: string;
}

/** Atomic small-file write (≤256 KB). `bytes` is raw bytes — this
 *  helper handles base64 encoding to dodge IPC binary-transfer
 *  awkwardness. For larger files use the regular transfer pipeline. */
export async function fsWriteBytes(
  addr: string,
  path: string,
  bytes: Uint8Array,
  createOnly = false,
): Promise<FsWriteBytesResult> {
  // Browser-side base64: btoa expects a binary string.
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  const bytesB64 = btoa(bin);
  return invoke<FsWriteBytesResult>("fs_write_bytes_run", {
    addr,
    path,
    bytesB64,
    createOnly,
  });
}

/** Convenience: write a UTF-8 string. */
export async function fsWriteText(
  addr: string,
  path: string,
  text: string,
  createOnly = false,
): Promise<FsWriteBytesResult> {
  return fsWriteBytes(addr, path, new TextEncoder().encode(text), createOnly);
}

/** Hand a game off to ShadowMount+ by appending its PS5-side source path
 *  (a game folder or a .ffpkg/.exfat/.ffpfs/.ffpfsc image) to SMP's watched
 *  `manual.lst`. SMP mounts + registers it on the next scan tick — so when SMP
 *  is running, ps5upload uses THIS instead of its own mount/register (which
 *  would race SMP for /user/app + app.db).
 *
 *  Read-modify-write of the small list file; idempotent (skips if the path is
 *  already listed). `added` is false when it was already present. SMP creates
 *  `/data/shadowmount/` itself, so this is only meaningful while SMP runs. */
export async function smpManualInstall(
  addr: string,
  path: string,
): Promise<{ added: boolean }> {
  let existing = "";
  try {
    const r = await fsReadPreview(addr, SMP_MANUAL_LIST_PATH);
    if (r.base64) {
      const bin = atob(r.base64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      existing = new TextDecoder().decode(bytes);
    }
  } catch {
    existing = ""; // list/dir may not exist yet — fsWriteText creates it
  }
  const next = appendManualListLine(existing, path);
  if (next === null) return { added: false };
  await fsWriteText(addr, SMP_MANUAL_LIST_PATH, next);
  return { added: true };
}

/** Mount a LWFS patch overlay (sceFsMountLwfs). Lets a title see
 *  patched files without a full reinstall. */
export async function lwfsMount(
  addr: string,
  patchPath: string,
  mountPoint?: string,
  titleId?: string,
): Promise<LwfsMountResult> {
  return invoke<LwfsMountResult>("lwfs_mount_run", {
    addr,
    patchPath,
    mountPoint: mountPoint ?? null,
    titleId: titleId ?? null,
  });
}

export interface Blake3HashResult {
  path: string;
  size: number;
  hash: string;
}

/** BLAKE3 hash a single PS5-side file (~2-3 s per GiB on PS5 UFS).
 *  Crypto-strength integrity check; for casual integrity use CRC32. */
export async function fsBlake3Hash(
  addr: string,
  path: string,
): Promise<Blake3HashResult> {
  return invoke<Blake3HashResult>("fs_blake3_hash", { addr, path });
}

/** Read up to 256 KB of a PS5-side file. Used by the FileSystem
 *  screen's preview pane for small files. */
export async function fsReadPreview(
  addr: string,
  path: string,
  maxBytes?: number,
): Promise<FsReadPreviewResult> {
  return invoke<FsReadPreviewResult>("fs_read_preview", {
    addr,
    path,
    maxBytes: maxBytes ?? null,
  });
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
 *  setting, only write new ones. Since 2.13.0 the payload's background
 *  watcher re-applies the threshold every 15s to defeat the firmware
 *  reset on app/game launch — see `payload/src/hw_info.c` (fan watcher). */
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

/** ShadowMountPlus metadata self-healer. The PS5-side background worker
 *  (payload/src/smp_meta.c) periodically copies missing icon0.png /
 *  pic*.png / param.json from /user/app/<TID>/sce_sys into
 *  /user/appmeta/<TID> to fix SMP's blank-tile failure mode. Off by
 *  default; the desktop opts in via `smpMetaControl({action:"start"})`,
 *  then polls `smpMetaStats` to render progress. */
export interface SmpMetaStats {
  running: boolean;
  poll_seconds: number;
  /** Unix seconds, 0 if no sweep has completed yet. */
  last_run_unix: number;
  games_scanned: number;
  icons_healed: number;
  pics_healed: number;
  json_healed: number;
  still_missing: number;
  /** Empty string when no game is unfixable. */
  last_missing: string;
}

export interface SmpMetaControlAck {
  ok: boolean;
  /** Post-clamp [5, 600] interval. Always populated. */
  poll_seconds: number;
  /** Sentinel; empty on success. Only known value:
   *  "pthread_create_failed". */
  err: string;
}

export type SmpMetaAction = "start" | "run_now" | "set_poll";

export async function smpMetaControl(
  transferAddr: string,
  action: SmpMetaAction,
  interval?: number,
): Promise<SmpMetaControlAck> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<SmpMetaControlAck>("ps5_smp_meta_control", {
    addr,
    action,
    interval: interval ?? null,
  });
}

export async function smpMetaStats(transferAddr: string): Promise<SmpMetaStats> {
  const addr = toMgmtAddr(transferAddr);
  return invoke<SmpMetaStats>("ps5_smp_meta_stats", { addr });
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
   *  (UFS2 volume), `ffpfs` = .ffpfs (PFS save-data volume,
   *  experimental). Null for game entries. Drives the label + mount
   *  behavior in the Library row. */
  imageFormat?: "exfat" | "ffpkg" | "ffpfs" | null;
  /** For `game` kind: PS5 title id (e.g. "PPSA00000") read from the
   *  game's sce_sys/param.json during scan. Used to dedup rows when
   *  the same game appears at multiple paths -- typically when a
   *  disk image is mounted AND its backing folder is also accessible
   *  on the filesystem. */
  titleId?: string | null;
  /** Last-modified time in seconds since the Unix epoch (st_mtime
   *  from the payload's lstat). 0 means the payload's stat failed
   *  for this entry, OR the running payload predates the field —
   *  treat as "unknown" and sort it to the end of date-sorted
   *  views. Used by the "Most recent first" / "Oldest first" sort
   *  options in the Library. */
  mtime?: number;
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
  // Walk every non-placeholder volume — the walker only ever reads
  // (list_dir, stat for sce_sys/param.json), so a read-only mount is
  // perfectly walkable. Pre-2.2.52 we filtered by `writable` too, which
  // hid games inside a UFS-DD .ffpkg whenever the PS5 kernel forced
  // MNT_RDONLY on the mount (Sony's UFS_DOWNLOAD_DATA image_type tends
  // to come back read-only on some firmwares regardless of the LVD-RW
  // flag we passed). The Move-modal still applies its own writable
  // filter; this list is just the scan roots.
  const volumes = (volumesRaw?.volumes ?? []).filter(
    (v) => !v.is_placeholder
  );
  // Set of volume roots — used by the walker to skip descending into
  // a path that's also a top-level volume root. Without this, a /data
  // walk and a /data/homebrew/PPSA17599 walk race for the shared
  // visited set and one short-circuits the other; whichever wins
  // determines whether the game inside surfaces. With it, the
  // dedicated mount walk always runs from a clean seed.
  const volumePathSet = new Set(volumes.map((v) => v.path));

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
  // Per-directory mtime captured from the parent's listing. We
  // emit game entries when we discover sce_sys/param.json *inside*
  // a directory, but the mtime we want is the *directory's* —
  // which we already saw in the parent's listing. This map
  // bridges the gap so the "Most recent first" sort works for
  // game folders too.
  const dirMtime = new Map<string, number>();

  const scanLimit = createScanLimiter(6);

  const listDir = async (
    path: string
  ): Promise<Array<{ name?: string; kind?: string; size?: number; mtime?: number }>> =>
    scanLimit(async () => {
      try {
        const res = await invoke<{
          entries?: Array<{
            name?: string;
            kind?: string;
            size?: number;
            mtime?: number;
          }>;
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
          const imageFormat: "exfat" | "ffpkg" | "ffpfs" | null = lower.endsWith(
            ".exfat"
          )
            ? "exfat"
            : lower.endsWith(".ffpkg")
              ? "ffpkg"
              : lower.endsWith(".ffpfs")
                ? "ffpfs"
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
              mtime: typeof e.mtime === "number" ? e.mtime : 0,
            });
          }
        } else if (e.kind === "dir") {
          dirNames.push(e.name);
          // Capture the directory's mtime now while we have the
          // parent listing in hand; emitted later when we discover
          // a game inside this directory (the game's "added"
          // timestamp is the folder's mtime, not the param.json's).
          if (typeof e.mtime === "number" && e.mtime > 0) {
            const childAbs = path === "/" ? `/${e.name}` : `${path}/${e.name}`;
            dirMtime.set(childAbs, e.mtime);
          }
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
            mtime: dirMtime.get(path) ?? 0,
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
        // Skip descending into another volume's root — it gets its
        // own dedicated walkVolume call. Without this, a /data walk
        // descending into /data/homebrew/PPSA17599 (where a .ffpkg
        // is mounted) would mark that path visited and short-circuit
        // the dedicated mount walk; the games inside would only
        // surface if the /data walk reached them under the entry cap.
        // The volumePathSet is captured at scanLibrary entry so the
        // skip stays consistent across concurrent walks.
        if (child !== volumePath && volumePathSet.has(child)) continue;
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

/** Configured engine base URL — usually the local sidecar, but may be a
 *  remote/self-hosted engine. Read per-call so a Settings change takes
 *  effect without a reload (used for `<img src>` icon URLs + game-meta). */
const engineBase = getEngineUrl;

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
  const url = `${engineBase()}/api/ps5/game-meta?addr=${encodeURIComponent(
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
  return `${engineBase()}/api/ps5/game-icon?addr=${encodeURIComponent(
    mgmt
  )}&path=${encodeURIComponent(path)}`;
}

/** How an installed title got onto the PS5:
 *   - "registered" — mounted/registered by ps5upload from a game folder,
 *                    `.exfat`/`.ffpkg` image, or upload (we track these via
 *                    /user/app/<id>/mount.lnk; `source` + `imageBacked` set).
 *   - "pkg"        — installed from a `.pkg` via Sony's installer (or shipped
 *                    with the console). `system: true` marks NPXS titles
 *                    (Store, Settings, …) that are dangerous to uninstall. */
export type InstalledAppOrigin = "registered" | "pkg";

export interface InstalledTitle {
  titleId: string;
  titleName: string;
  origin: InstalledAppOrigin;
  /** registered + disk-image-backed (.exfat/.ffpkg) vs plain folder. */
  imageBacked: boolean;
  /** registered: the source path we registered from (empty otherwise). */
  source: string;
  /** NPXS-prefixed system title — UI greys it and double-confirms uninstall. */
  system: boolean;
}

export interface InstalledAppsResult {
  titles: InstalledTitle[];
  /** True if the payload couldn't report the "registered" set (older
   *  payload); the UI then shows everything as "installed" with a note. */
  registeredUnavailable: boolean;
}

/** List every installed title on the PS5, grouped by origin. Routes through
 *  the engine's `/api/ps5/apps/installed` (filesystem enumeration of
 *  /user/appmeta — no sqlite, safe on FW 9.60). */
export async function appsInstalled(
  transferAddr: string,
): Promise<InstalledAppsResult> {
  const addr = toMgmtAddr(transferAddr);
  const res = await invoke<{
    titles?: Array<{
      title_id?: string;
      title_name?: string;
      origin?: string;
      image_backed?: boolean;
      source?: string;
      system?: boolean;
    }>;
    registered_unavailable?: boolean;
  }>("ps5_apps_installed", { addr });
  const titles: InstalledTitle[] = (res?.titles ?? [])
    .filter((t): t is { title_id: string } & typeof t => !!t.title_id)
    .map((t) => ({
      titleId: t.title_id,
      titleName: t.title_name && t.title_name.trim() ? t.title_name : t.title_id,
      origin: t.origin === "registered" ? "registered" : "pkg",
      imageBacked: !!t.image_backed,
      source: t.source ?? "",
      system: !!t.system,
    }));
  return { titles, registeredUnavailable: !!res?.registered_unavailable };
}

/** Whether the console is settled enough to take a .pkg install — the engine
 *  round-trips the AppListRegistered frame, which goes unanswered while the
 *  console is recovering from a prior install (the post-install SceShellUI
 *  black-screen blip). Returns false on any error (treat "can't tell" as
 *  "not ready" so the caller waits rather than firing into the blip). */
export async function consoleReadiness(host: string): Promise<boolean> {
  try {
    const res = await invoke<{ ready?: boolean }>("ps5_readiness", {
      addr: toMgmtAddr(host),
    });
    return !!res?.ready;
  } catch {
    return false;
  }
}

/** One `.pkg` found on a connected external/USB drive. */
export interface ExternalPkg {
  /** Absolute on-console path, e.g. `/mnt/usb0/games/foo.pkg`. */
  path: string;
  /** The drive mount it was found under (`/mnt/usb0`, `/mnt/ext1`). */
  drive: string;
  /** Basename. */
  name: string;
  size: number;
  /** ContentID (empty for `\x7FFIH` / unreadable headers). */
  contentId: string;
  /** Title id (CUSA…/PPSA…) derived from the content id. */
  titleId: string;
  /** "ps4" | "ps5" | "" — from header magic + title-id prefix. */
  platform: string;
}

/** List `.pkg` files on connected external/USB drives. These install in place
 *  (the engine copies the file to /user/data first — Sony's installer can't
 *  read the exfat USB mount directly — then runs the normal install cascade),
 *  so they need no upload from the desktop. */
export async function pkgScanExternal(transferAddr: string): Promise<ExternalPkg[]> {
  const addr = toMgmtAddr(transferAddr);
  const res = await invoke<{
    packages?: Array<{
      path?: string;
      drive?: string;
      name?: string;
      size?: number;
      content_id?: string;
      title_id?: string;
      platform?: string;
    }>;
  }>("pkg_scan_external", { addr });
  return (res?.packages ?? [])
    .filter((p): p is { path: string } & typeof p => !!p.path)
    .map((p) => ({
      path: p.path,
      drive: p.drive ?? "",
      name: p.name ?? p.path.split("/").pop() ?? p.path,
      size: p.size ?? 0,
      contentId: p.content_id ?? "",
      titleId: p.title_id ?? "",
      platform: p.platform ?? "",
    }));
}

/** Authoritative metadata for one on-console pkg, parsed from its PARAM.SFO.
 *  Fields are best-effort — empty when the SFO is missing or the pkg uses the
 *  unreadable `\x7FFIH` header. */
export interface PkgConsoleMetadata {
  contentId: string;
  title: string;
  titleId: string;
  /** PARAM.SFO CATEGORY: "gd" (base) / "gp" (update) / "ac" (DLC) / …. */
  category: string;
  /** PARAM.SFO APP_VER, e.g. "01.04". */
  appVer: string;
  platform: string;
}

/** Parse one on-console pkg (e.g. on `/mnt/usb0`) for its title, version
 *  (APP_VER), category, and content id — via a few ranged reads on the engine.
 *  Lazily enriches the External Packages rows the fast scan leaves sparse.
 *  Returns null on error so callers can simply skip enrichment for that row. */
export async function pkgMetadataConsole(
  transferAddr: string,
  path: string,
): Promise<PkgConsoleMetadata | null> {
  const addr = toMgmtAddr(transferAddr);
  try {
    const m = await invoke<{
      content_id?: string;
      title?: string;
      title_id?: string;
      category?: string;
      app_ver?: string;
      platform?: string;
    }>("pkg_metadata_console", { addr, path });
    return {
      contentId: m?.content_id ?? "",
      title: m?.title ?? "",
      titleId: m?.title_id ?? "",
      category: m?.category ?? "",
      appVer: m?.app_ver ?? "",
      platform: m?.platform ?? "",
    };
  } catch {
    return null;
  }
}

/** Stable `<img src=...>` URL for an installed title's cover art
 *  (/user/appmeta/<titleId>/icon0.png), streamed back as `image/png` by
 *  the engine. Works identically on desktop and Android (in-process engine
 *  on the same loopback port; CSP allows 127.0.0.1:19113 for img-src). */
export function appIconUrl(transferAddr: string, titleId: string): string {
  const mgmt = toMgmtAddr(transferAddr);
  return `${engineBase()}/api/ps5/app-icon?addr=${encodeURIComponent(
    mgmt
  )}&title_id=${encodeURIComponent(titleId)}`;
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

/** Pick the volume a given on-PS5 destination path belongs to by
 *  longest-prefix match. E.g. for `dest = "/mnt/ext0/games/big.pkg"`
 *  and a volume list containing `/`, `/data`, `/mnt/ext0`, we pick
 *  `/mnt/ext0` (the deepest mount that's a prefix of the dest). Returns
 *  `null` when no volume covers the path — caller should treat that as
 *  "unknown free space" and skip the pre-flight check rather than
 *  refusing the upload outright. */
export function volumeForPath(
  volumes: readonly Volume[],
  destPath: string,
): Volume | null {
  let best: Volume | null = null;
  for (const v of volumes) {
    if (!v.path) continue;
    // Match either an exact path or a path with the volume as its
    // strict prefix segment. A volume `/data` should match
    // `/data/foo` but NOT `/database/foo`.
    const sep = v.path.endsWith("/") ? "" : "/";
    const isPrefix =
      destPath === v.path || destPath.startsWith(`${v.path}${sep}`);
    if (!isPrefix) continue;
    if (!best || v.path.length > best.path.length) best = v;
  }
  return best;
}

export interface FreeSpaceCheck {
  /** Volume the destination resolves to. `null` if none of the
   *  reported volumes prefix the path — pre-flight skipped. */
  volume: Volume | null;
  /** Bytes free on the resolved volume. `null` when `volume` is
   *  null. */
  freeBytes: number | null;
  /** True iff `freeBytes !== null` AND `freeBytes < requiredBytes`. */
  insufficient: boolean;
  /** Bytes the caller needs to free up. 0 when sufficient or
   *  unknown. */
  shortBy: number;
}

/** Pre-flight check: does the destination drive have enough room for
 *  `requiredBytes`? Returns a structured result the caller can render
 *  as either a hard block ("free up X more bytes") or a soft warning
 *  ("we couldn't determine free space, proceeding anyway").
 *
 *  Defensive on errors — a `ps5_volumes` failure resolves to a
 *  "unknown" result, not an exception, so a missing payload or
 *  network blip doesn't gate the user out of an upload they could
 *  have completed. */
export async function checkDestinationFreeSpace(
  transferAddr: string,
  destPath: string,
  requiredBytes: number,
): Promise<FreeSpaceCheck> {
  try {
    const volumes = await fetchVolumes(transferAddr);
    const volume = volumeForPath(volumes, destPath);
    if (!volume) {
      return { volume: null, freeBytes: null, insufficient: false, shortBy: 0 };
    }
    const freeBytes = volume.free_bytes;
    const insufficient = freeBytes < requiredBytes;
    return {
      volume,
      freeBytes,
      insufficient,
      shortBy: insufficient ? requiredBytes - freeBytes : 0,
    };
  } catch {
    // Network blip / payload unreachable — degrade to "unknown" so
    // the caller doesn't refuse an upload that might succeed.
    return { volume: null, freeBytes: null, insufficient: false, shortBy: 0 };
  }
}

// `toMgmtAddr` migrated to `lib/addr.ts::mgmtAddr` in 2.12.0 — see
// the head of that module for the rationale (canonical address helpers,
// kills the "two `toMgmtAddr` with different signatures" footgun).
// The shim below is kept as a local re-export so the existing
// `toMgmtAddr(transferAddr)` call sites in this file compile without
// every line being touched in this PR. New code should import
// `mgmtAddr` directly from `lib/addr`.
import { mgmtAddr as _mgmtAddr } from "../lib/addr";
const toMgmtAddr = _mgmtAddr;

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
  /** Per-file progress (Running, multi-file uploads). Climbs as the
   *  engine reads each source file into a pack frame (one bump per file).
   *  Smoother than deriving file-count from bytes_sent, which jumps in
   *  ~200-file chunks on packed-shard ACKs and looked like
   *  "start → finished" on 46k-file game folders. `undefined` (or 0)
   *  means the upload path doesn't report it; the UI falls back to its
   *  size-derived estimate. */
  files_processing?: number;
  /** P3 / v2.18.0: files the payload has fully committed during the
   *  post-100% COMMIT_TX apply loop. Ticks up from 0 to
   *  `files_finalizing_total` as APPLY_PROGRESS frames arrive from
   *  new payloads (those that recognise TX_FLAG_APPLY_PROGRESS_REQUESTED,
   *  which the engine sets on every multi-file BEGIN_TX). UI surfaces
   *  this as a "Finalized N of M files" counter on the running banner
   *  so users see motion through the 10-30 min commit phase that used
   *  to be a silent black box. `undefined` (or 0) on old payloads
   *  that don't emit progress — UI falls back to the plain "Finalizing
   *  on PS5…" pill from v2.17.3. */
  files_finalized?: number;
  /** P3 / v2.18.0: total files the payload will commit. Surfaced as a
   *  paired denominator for `files_finalized`. 0 outside the finalize
   *  phase. */
  files_finalizing_total?: number;
  /** P3 / v2.18.0: cumulative bytes finalized during commit-apply.
   *  Second progress dimension alongside file count; useful when file
   *  sizes vary wildly. */
  bytes_finalized?: number;
  /** Files actually sent (Done only). */
  files_sent?: number;
  shards_sent?: number;
  dest?: string;
  error?: string;
  /** Machine-parseable error category lifted from the payload's
   *  error frame body. Populated alongside `error` when the failure
   *  originated from a PS5 protocol error frame. UI uses this for
   *  humanized rendering (e.g. `direct_writer_io_error` → "PS5 is
   *  out of free space"). `undefined` for local-side / non-payload
   *  errors — fall back to `error` text. */
  error_reason?: string;
  /** Human-readable detail string lifted from the payload's error
   *  frame `"detail"` field. Often pinpoints the on-PS5 path or
   *  underlying errno. Shown as a secondary line under the
   *  humanized title. */
  error_detail?: string;
}

/** Job-failure exception carrying the structured `error_reason` +
 *  `error_detail` from a failed `JobSnapshot`. Callers should catch
 *  this in preference to a plain `Error` when they want to surface
 *  the humanized message; `instanceof UploadJobError` lets the UI
 *  branch on whether structured fields are available. */
export class UploadJobError extends Error {
  reason?: string;
  detail?: string;
  constructor(message: string, reason?: string, detail?: string) {
    super(message);
    this.name = "UploadJobError";
    this.reason = reason;
    this.detail = detail;
  }
}

/** Humanize a payload's `error_reason` token into a one-line message
 *  the user can act on. Returns `null` when the reason is unknown
 *  (caller falls back to the raw `error` field). The hint is paired
 *  in the UI with the raw `error_detail` for diagnostic depth. */
export function humanizeJobErrorReason(reason: string | undefined): string | null {
  if (!reason) return null;
  // Mid-transfer write failure from the payload's direct-write path. The
  // payload sends `fs_write_failed_errno_<N>` (e.g. 28=ENOSPC, 27=EFBIG)
  // — previously these write failures closed the socket with no error
  // frame, so the app showed the misleading "PS5 stopped responding /
  // crashed" message. Map the disk-space codes pointedly; anything else
  // (and the bare `fs_write_failed` fallback) gets generic guidance.
  if (reason.startsWith("fs_write_failed")) {
    if (reason === "fs_write_failed_errno_28" || reason === "fs_write_failed_errno_27") {
      return "The destination drive ran out of space (or the file is too big for that filesystem). Free space on the PS5 / external drive — or pick a different destination — then click Retry.";
    }
    return "The PS5 couldn't write to the destination mid-transfer — most often the drive filled up or an external drive disconnected. Check free space / reconnect the drive, then click Retry (the upload resumes from where it stopped).";
  }
  switch (reason) {
    case "preflight_insufficient_space":
      return "The destination drive doesn't have enough free space for this file. Free up space on the PS5 (Settings → Storage) or pick a different destination, then click Retry.";
    case "direct_writer_io_error":
      return "The PS5 ran out of free space (or an external drive disconnected) while writing the file. Free up space on the destination drive and click Retry — the upload resumes from where it stopped.";
    case "direct_tx_corrupt":
      return "The PS5 detected protocol corruption on this transfer. Restart the payload from the Send Payload tab and retry.";
    case "packed_unsupported":
      return "The PS5 helper rejected a packed transfer — this happens with an outdated payload when a folder upload comes down to a single small file (e.g. a Resume of a game with many tiny files). Update ps5upload to the latest version, re-send the payload from the Send Payload tab, then retry.";
    case "size_mismatch":
    case "shards_incomplete":
    case "spool_apply_failed":
      return "The transfer didn't finish before being interrupted — a file on the PS5 is incomplete, so it wasn't published (your old copy, if any, is untouched). This usually means the PS5 went into rest mode or lost power mid-upload. Keep the console awake (Settings → System → Power Saving → Set Time Until PS5 Turns Off), then re-run this item — Resume now re-sends only the missing files, or choose Override for a clean copy.";
    case "fs_delete_path_not_allowed":
    case "fs_mkdir_path_not_allowed":
    case "fs_list_dir_path_denied":
      return "PS5 refused access to that path. Use /data/, /user/, or a mounted /mnt/ext*, /mnt/usb* path.";
    case "tx_table_full":
      return "Too many simultaneous transfers in flight on the PS5. Wait for some to finish or restart the payload.";
    default:
      return null;
  }
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

/** Truly stop a running transfer job. The engine flips its cancel flag and the
 *  transfer aborts at its next shard boundary (partial tx left resumable).
 *  Idempotent + best-effort: a finished/unknown job just returns cancelled:false. */
export async function jobCancel(jobId: string): Promise<void> {
  await invoke("job_cancel", { jobId });
}

export async function waitForJob(
  jobId: string,
  intervalMs = 500,
): Promise<JobSnapshot> {
  while (true) {
    const snap = await jobStatus(jobId);
    if (snap.status === "done") return snap;
    if (snap.status === "failed") {
      throw new UploadJobError(
        snap.error ?? "job failed",
        snap.error_reason,
        snap.error_detail,
      );
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
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
  /** Whether the payload's process-wide ucred elevation succeeded.
   *  True = kernel R/W primitive available; the privileged BGFT
   *  Int-family symbols (and pkg install) will work. False (or
   *  null) = the loader didn't grant kernel R/W; install will fail
   *  even with the right symbols resolved. Surfaced on the
   *  Connection screen so the user sees the prerequisite state. */
  ucredElevated: boolean | null;
  /** Max parallel upload streams this payload will service concurrently
   *  (from STATUS_ACK `max_transfer_streams`). Absent on payloads that
   *  predate multi-stream → null, which the caller treats as 1 (single
   *  stream). The Upload path resolves the actual count as
   *  min(user setting, this). See docs/multistream-upload.md. */
  maxTransferStreams: number | null;
  /** Raw error string from the engine when reachable=false. Lets the
   *  Connection screen's wait-for-boot banner surface what actually
   *  went wrong (TCP connect refused, STATUS_ACK timeout, etc.)
   *  instead of just "didn't come up". null when reachable. */
  error: string | null;
}> {
  const resp = await invoke<{
    reachable?: boolean;
    loaded?: boolean;
    error?: string;
    status?: {
      version?: string;
      ps5_kernel?: string;
      ucred_elevated?: boolean;
      max_transfer_streams?: number;
    };
  }>("payload_check", { ip });
  return {
    reachable: !!resp?.reachable,
    loaded: !!resp?.loaded,
    payloadVersion: resp?.status?.version ?? null,
    ps5Kernel: resp?.status?.ps5_kernel ?? null,
    ucredElevated:
      typeof resp?.status?.ucred_elevated === "boolean"
        ? resp.status.ucred_elevated
        : null,
    maxTransferStreams:
      typeof resp?.status?.max_transfer_streams === "number" &&
      resp.status.max_transfer_streams > 0
        ? resp.status.max_transfer_streams
        : null,
    error: resp?.reachable ? null : resp?.error ?? null,
  };
}
