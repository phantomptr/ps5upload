import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { invoke } from "@tauri-apps/api/core";

import { fsListDir, fsDelete, fsMkdir } from "../api/ps5";
import { hostOf, mgmtAddr, transferAddr } from "../lib/addr";
import { humanizePs5Error } from "../lib/humanizeError";
import {
  stagingBasename,
  stagingSubdirForCategory,
  categoryForSubdir,
} from "../lib/pkgStagingPath";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { transferScreenBusy } from "../lib/ps5Transfers";
import { useInstallSettingsStore } from "./installSettings";
import { useConnectionStore } from "./connection";
import { parsePS5Firmware } from "../lib/ps5Firmware";

/**
 * Package Library — the model behind the redesigned Install Package screen.
 *
 * Unlike the old transient install queue (which uploaded a `.pkg`, installed
 * it, then deleted it), the library treats the PS5 as durable storage: a
 * `.pkg` you add is uploaded ONCE into `/user/data/ps5upload/pkg_library/`
 * and stays there until you delete it, so you can (re)install it any time
 * without re-uploading. The source of truth is the on-PS5 directory listing,
 * not local state — so the list survives app restarts.
 *
 * Install PRIMARY path (since 2.25.2): the main payload's
 * `sceAppInstUtilInstallByPackage` run in the full jailbreak context, which
 * installs LAUNCHABLE content. If that's rejected we fall back to the DPI
 * daemon on :9040 (also InstallByPackage since 2.25.2), then re-send the main
 * payload (the single-payload loader swapped it out). The payload's own tier
 * ladder (bgft.c) only drops to the unlaunchable `sceAppInstUtilAppInstallPkg`
 * as an absolute last resort, flagging it so we warn the user — see
 * `pkgInstallMayNotLaunch`.
 *
 * IMPORTANT: the library dir is deliberately NOT `pkg_temp/` — the payload
 * sweeps that on boot (`runtime_sweep_stale_pkg_temp`), which would wipe the
 * user's library. `pkg_library/` is left untouched.
 */
export const PKG_LIBRARY_DIR = "/user/data/ps5upload/pkg_library";

/** Warning copy shown when an install lands via the unlaunchable last-resort
 *  path (register_path "appinst-local"); see `pkgInstallMayNotLaunch`. */
export const PKG_MAY_NOT_LAUNCH_MESSAGE =
  "Installed, but via a fallback that may not launch on this firmware. If the game won't start (“can't start the game or app”), re-install it from the PS5: Settings → System → Debug Settings → Game → Package Installer.";

/** Whether an install response indicates the title may not launch. True only
 *  when the engine flags `may_not_launch`, or (older engines that don't send
 *  the flag) when the payload reports the unlaunchable `appinst-local`
 *  last-resort path. Every launchable tier (appinst / shellui-rpc / intdebug /
 *  regular) → false. */
export function pkgInstallMayNotLaunch(r: {
  register_path?: string;
  may_not_launch?: boolean;
}): boolean {
  return r.may_not_launch ?? r.register_path === "appinst-local";
}

/** The lastResult for a SUCCESSFUL primary install — amber warn when the title
 *  may not launch, plain green success otherwise. */
export function installedLastResult(mayNotLaunch: boolean): {
  ok: true;
  message: string;
  warn?: boolean;
} {
  return mayNotLaunch
    ? { ok: true, warn: true, message: PKG_MAY_NOT_LAUNCH_MESSAGE }
    : { ok: true, message: "Installed." };
}

export type PkgStatus = "idle" | "queued" | "uploading" | "installing";

export interface PkgEntry {
  /** Filename on the PS5, e.g. `CUSA00207.pkg`. Unique within the dir. */
  name: string;
  /** Absolute PS5 path (`PKG_LIBRARY_DIR/name`). The install/delete key. */
  path: string;
  /** Size in bytes — from the dir listing, or the local file while uploading. */
  size: number;
  /** ContentID (from the filename, which we name `<ContentID>.pkg`, or from
   *  parsed metadata while uploading). May be empty for oddly-named files. */
  contentId: string;
  /** Friendly title, when known (parsed at upload, cached in localStorage). */
  title?: string;
  /** Title id (PPSA/CUSA…) derived from the ContentID. Drives cover art and
   *  the "installed" badge. Undefined when it can't be derived. */
  titleId?: string;
  /** PARAM.SFO category — `gd` (base game), `gp` (update/patch), `ac` (DLC).
   *  A base and its update share a ContentID, so this is the only thing that
   *  tells them apart. On upload it comes from the parsed header; on refresh
   *  it's inferred from the staging sub-directory the file lives in. Undefined
   *  for root-level files of unknown category. Drives the Update/DLC badge. */
  category?: string;
  /** Transient per-row state (never persisted; recomputed each session). */
  status: PkgStatus;
  /** Bytes transferred so far (upload) — drives the row progress bar. */
  bytes?: number;
  /** Total bytes for the active upload. */
  totalBytes?: number;
  /** Outcome of the last install attempt this session, for inline feedback.
   *  `warn` marks a soft-success: the install was accepted but via the
   *  unlaunchable last-resort path (`register_path === "appinst-local"`), so
   *  the title may not start on some firmwares (notably FW 12.xx). Rendered
   *  amber rather than green so the user knows to re-install via the PS5's
   *  Settings → Package Installer if it won't boot. */
  lastResult?: { ok: boolean; message: string; warn?: boolean };
}

/** PS5 title ids look like `CUSA12345` / `PPSA01234` — four letters then five
 *  digits. A ContentID embeds one as its second dash-segment, e.g.
 *  `EP9000-CUSA00207_00-BLOODBORNE000000`. Returns it, or null. Exported for
 *  unit testing. */
export function titleIdFromContentId(contentId: string): string | null {
  if (!contentId) return null;
  const seg = contentId.split("-")[1]; // "CUSA00207_00"
  const id = seg?.split("_")[0] ?? "";
  return /^[A-Z]{4}\d{5}$/.test(id) ? id : null;
}

/** ContentID from a `<ContentID>.pkg` filename (strip the extension). */
function contentIdFromName(name: string): string {
  return name.toLowerCase().endsWith(".pkg") ? name.slice(0, -4) : name;
}

// ── Title metadata cache ──────────────────────────────────────────────
// Filenames are `<ContentID>.pkg`, so the list always has the ContentID, but
// not a friendly title. We capture the parsed title at upload time and cache
// it (contentId → title) so rows show real names. A cache miss (uploaded on
// another machine, or cleared storage) just falls back to the ContentID.
const TITLE_CACHE_KEY = "ps5upload.pkg_library.titles.v1";

function loadTitleCache(): Record<string, string> {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(TITLE_CACHE_KEY);
    const parsed = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function cacheTitle(contentId: string, title: string): void {
  if (!contentId || !title) return;
  try {
    const c = loadTitleCache();
    c[contentId] = title;
    window.localStorage.setItem(TITLE_CACHE_KEY, JSON.stringify(c));
  } catch {
    /* best-effort */
  }
}

interface SplitParseResponse {
  parts?: string[];
  total_size?: number;
  head?: {
    content_id?: string;
    title?: string;
    /** PARAM.SFO CATEGORY ("gd"/"gp"/"ac"). Already present in the
     *  engine's parse-split response (it serialises the full PkgMetadata);
     *  we just hadn't been reading it. */
    category?: string;
    warnings?: string[];
  };
}

function pkgError(e: unknown): string {
  return humanizePs5Error(e instanceof Error ? e.message : String(e));
}

interface PkgLibraryState {
  /** Library contents, derived from the on-PS5 dir + transient row state. */
  entries: PkgEntry[];
  loading: boolean;
  error: string | null;
  /** True while an install is mid-flight (including the time it spends queued
   *  behind an active upload). Installs swap the payload in/out, so the UI
   *  blocks refresh/other installs until it settles. */
  installing: boolean;
  /** Human-readable "what's happening" line shown while an install or .pkg
   *  upload is QUEUED behind an active transfer (the PS5 can only do one at a
   *  time). Null when nothing is waiting. */
  busyNotice: string | null;

  refresh: (host: string) => Promise<void>;
  addAndUpload: (localPath: string, host: string) => Promise<void>;
  install: (path: string, host: string) => Promise<void>;
  /** Abandon an install that's still waiting its turn behind an upload. */
  cancelPendingInstall: () => void;
  remove: (path: string, host: string) => Promise<void>;
  /** Delete (from the PS5) every staged package that has already been
   *  installed successfully — clears the spent-package clutter without
   *  touching anything mid-flight or not-yet-installed. */
  clearFinished: (host: string) => Promise<void>;
  /** Delete (from the PS5) every idle staged package — a full wipe of the
   *  staging library. Skips rows that are uploading/installing/queued. */
  clearAll: (host: string) => Promise<void>;
}

/** A library row is "finished" when it's settled (idle) AND its last
 *  install succeeded — i.e. a spent staged package safe to delete. */
export function isFinishedPkg(e: PkgEntry): boolean {
  return e.status === "idle" && e.lastResult?.ok === true;
}

/** Merge a freshly-listed set with the current rows, preserving transient
 *  status (a row mid-upload/install must not be clobbered by a refresh). */
function mergeListing(prev: PkgEntry[], listed: PkgEntry[]): PkgEntry[] {
  const byPath = new Map(prev.map((e) => [e.path, e]));
  // Keep active (uploading) rows that aren't on disk yet.
  const active = prev.filter(
    (e) => e.status !== "idle" && !listed.some((l) => l.path === e.path),
  );
  const merged = listed.map((l) => {
    const old = byPath.get(l.path);
    if (old && old.status !== "idle") {
      // Preserve in-flight state but adopt the real on-disk size.
      return { ...old, size: l.size || old.size };
    }
    // Carry forward last install result for an idle row.
    return old?.lastResult ? { ...l, lastResult: old.lastResult } : l;
  });
  // `||` (not `??`) so an empty-string contentId (headerless pkg) falls
  // through to the filename instead of sorting as "".
  const label = (e: PkgEntry) => (e.title || e.contentId || e.name).toLowerCase();
  return [...active, ...merged].sort((a, b) => label(a).localeCompare(label(b)));
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export interface PkgInstallOutcome {
  /** The install registered with Sony's installer (rc==0 or DPI ok). */
  installed: boolean;
  /** Installed only via the unlaunchable last-resort path (may not start on
   *  some firmware, notably FW 12.xx). Surface a caution, not a clean OK. */
  mayNotLaunch: boolean;
  /** First error seen (empty when installed cleanly). */
  errMessage: string;
}

/**
 * The bare `.pkg` install mechanism, with NO store/UI side effects — shared by
 * the Install Package screen's manual install (`install()`) and the upload
 * queue's pkg finisher (`uploadQueue.runOne`). The `.pkg` must already be
 * staged on the PS5 at `localPs5Path`.
 *
 * Cascade (HW-proven): the MAIN PAYLOAD's InstallByPackage first — it runs in
 * the full jailbreak context, which is what actually installs LAUNCHABLE
 * content into /user/app/<title>. Only if the firmware rejects that do we fall
 * back to the standalone DPI daemon on :9040 (registers metadata, may not be
 * launchable on newer firmware), then restore the main payload. Returns whether
 * it installed, whether it's the unlaunchable path, and the first error.
 *
 * Throws only if the DPI daemon itself can't come up after a main-payload
 * reject (a genuine dead-end) — callers should catch and treat as failure.
 */
export async function runPkgInstall(
  host: string,
  localPs5Path: string,
  contentId: string | null,
): Promise<PkgInstallOutcome> {
  const ip = hostOf(host);
  let installed = false;
  let mainErr = "";
  let mayNotLaunch = false;
  try {
    const r = (await invoke("pkg_install_start", {
      ps5Addr: mgmtAddr(host),
      path: null,
      splitRoot: null,
      packageTypeOverride: null,
      localPs5Path,
      contentId: contentId || null,
    })) as {
      err_code?: number;
      register_path?: string;
      err_message?: string;
      may_not_launch?: boolean;
    };
    const rc = (r.err_code ?? 0) >>> 0;
    if (rc === 0) {
      installed = true;
      mayNotLaunch = pkgInstallMayNotLaunch(r);
    } else {
      mainErr = r.err_message || `0x${rc.toString(16).padStart(8, "0")}`;
    }
  } catch (e) {
    mainErr = pkgError(e);
  }

  if (!installed) {
    // FALLBACK: the jailbroken-context install was rejected. Try the
    // standalone DPI daemon (replaces our payload on the single-payload
    // loader, installs, then we restore the payload).
    const ens = (await invoke("dpi_ensure", { ip })) as {
      ok?: boolean;
      error?: string;
    };
    if (!ens.ok) {
      throw new Error(
        ens.error ||
          `Main-payload install failed (${mainErr}) and the DPI daemon didn't come up on :9040`,
      );
    }
    let resp: { ok?: boolean; rc?: number; err_message?: string } = {};
    try {
      resp = (await invoke("pkg_dpi_install", {
        ps5Addr: mgmtAddr(host),
        localPs5Path,
      })) as typeof resp;
    } catch (e) {
      resp = { ok: false, rc: 0, err_message: pkgError(e) };
    }
    // Restore our main payload — the daemon replaced it. Best-effort.
    try {
      const bp = (await invoke("payload_bundled_path")) as {
        ok?: boolean;
        path?: string;
      };
      if (bp?.ok && bp.path) {
        await invoke("payload_send", { ip, path: bp.path, port: null });
      }
    } catch {
      /* best-effort restore */
    }
    installed = !!resp.ok;
    if (!installed) {
      const rc = (resp.rc ?? 0) >>> 0;
      mainErr =
        resp.err_message ||
        mainErr ||
        `Install was rejected (0x${rc.toString(16).padStart(8, "0")}).`;
    }
  }
  return { installed, mayNotLaunch, errMessage: mainErr };
}

/**
 * One isolated PkgLibrary store per PS5 console (see the registry below). The
 * store body is unchanged from the old single-instance design — making it a
 * factory is what gives each console its own `entries` + `installing` state, so
 * installing on console A never blocks console B. Every method still takes
 * `host` (it matches this instance's console) and uses it for addresses.
 */
const makePkgLibraryStore = () =>
  createStore<PkgLibraryState>((set, get) => ({
  entries: [],
  loading: false,
  error: null,
  installing: false,
  busyNotice: null,

  async refresh(host) {
    if (!host?.trim() || get().installing) return;
    set({ loading: true, error: null });
    const addr = mgmtAddr(host);
    const titles = loadTitleCache();
    try {
      // List one dir, tolerating ENOENT (errno 2 — dir not created yet =
      // empty, not an error). `strict` re-throws any OTHER error so an
      // offline/refused console surfaces instead of silently wiping the
      // list; we use it only for the root dir. Sub-dir scans are
      // best-effort — a successful root list already proved reachability,
      // so a stray sub-dir error shouldn't blank the whole library.
      const listOne = async (dir: string, strict: boolean) => {
        try {
          return await fsListDir(addr, dir);
        } catch (err) {
          const msg = err instanceof Error ? err.message : String(err);
          if (/fs_list_dir_opendir_errno_2\b/.test(msg)) return [];
          if (strict) throw err;
          return [];
        }
      };
      // Base/unknown live at the library root; updates + DLC in their own
      // sub-dirs (see lib/pkgStagingPath). Scan all three so a base and its
      // update both show, each badged by the dir it came from.
      const SCAN = ["", "updates", "dlc"];
      const entries: PkgEntry[] = [];
      for (const subdir of SCAN) {
        const dir = subdir ? `${PKG_LIBRARY_DIR}/${subdir}` : PKG_LIBRARY_DIR;
        const listed = await listOne(dir, subdir === "");
        for (const e of listed) {
          if (e.kind !== "file" || !e.name.toLowerCase().endsWith(".pkg")) {
            continue;
          }
          const contentId = contentIdFromName(e.name);
          entries.push({
            name: e.name,
            path: `${dir}/${e.name}`,
            size: e.size,
            contentId,
            title: titles[contentId],
            titleId: titleIdFromContentId(contentId) ?? undefined,
            category: categoryForSubdir(subdir),
            status: "idle" as PkgStatus,
          });
        }
      }
      set({ entries: mergeListing(get().entries, entries), loading: false });
    } catch (e) {
      set({ error: pkgError(e), loading: false });
    }
  },

  async addAndUpload(localPath, host) {
    if (!host?.trim()) return;
    // An install swaps the main payload out (DPI), which kills the transfer
    // port — never start an upload while one is running.
    if (get().installing) {
      set({ error: "Can't upload while an install is in progress." });
      return;
    }
    set({ error: null });
    // 1. Parse the local .pkg header for ContentID + title, and reject inputs
    //    DPI can't take.
    let meta: SplitParseResponse;
    try {
      meta = (await invoke("pkg_metadata_split", {
        path: localPath,
      })) as SplitParseResponse;
    } catch (e) {
      set({ error: `Couldn't read .pkg header: ${pkgError(e)}` });
      return;
    }
    if ((meta.parts?.length ?? 1) > 1) {
      set({
        error:
          "Split .pkg sets aren't supported by the DPI installer — pick the single lead .pkg.",
      });
      return;
    }
    const contentId = meta.head?.content_id ?? "";
    const title = meta.head?.title;
    const category = meta.head?.category;
    const totalBytes = meta.total_size ?? 0;
    if (title) cacheTitle(contentId, title);

    // 2. Name the on-PS5 file `<ContentID>.pkg` (Sony's installer keys on the
    //    basename matching the ContentID — see lib/pkgStagingPath). A base
    //    game and its update/DLC share that ContentID, so they're routed to
    //    distinct sub-directories (basename unchanged) to keep them from
    //    overwriting each other in the library.
    const basename = stagingBasename(
      contentId,
      Math.random().toString(36).slice(2),
      Date.now(),
    );
    const subdir = stagingSubdirForCategory(category);
    const destPath = subdir
      ? `${PKG_LIBRARY_DIR}/${subdir}/${basename}`
      : `${PKG_LIBRARY_DIR}/${basename}`;

    // Refuse to re-add a pkg that's already uploading to the same path:
    // two concurrent transfers to one file would corrupt it, and the two
    // poll loops would fight over the same row's progress. (A headerless
    // pkg gets a unique random basename each time, so this only triggers
    // for a real ContentID being added twice mid-upload.)
    if (
      get().entries.some(
        (e) => e.path === destPath && e.status === "uploading",
      )
    ) {
      set({ error: `${title || contentId || basename} is already uploading.` });
      return;
    }

    // The transfer port (:9113) is single-client: another upload (from the
    // Upload screen or another .pkg here) must finish before this one starts,
    // or they collide on the port. `othersBusy` is true while any such
    // transfer holds it.
    const othersBusy = () =>
      transferScreenBusy(host) ||
      get().entries.some(
        (e) => e.path !== destPath && e.status === "uploading",
      );

    // 3. Optimistic row — "queued" if it has to wait for the port, else
    //    straight to "uploading".
    const optimistic: PkgEntry = {
      name: basename,
      path: destPath,
      size: totalBytes,
      contentId,
      title,
      titleId: titleIdFromContentId(contentId) ?? undefined,
      category,
      status: othersBusy() ? "queued" : "uploading",
      bytes: 0,
      totalBytes,
    };
    set({
      entries: [
        optimistic,
        ...get().entries.filter((e) => e.path !== destPath),
      ],
    });

    const patch = (p: Partial<PkgEntry>) =>
      set({
        entries: get().entries.map((e) =>
          e.path === destPath ? { ...e, ...p } : e,
        ),
      });

    // 4. Upload over the bulk-transfer port, polling job_status for progress.
    try {
      // Wait our turn on the single-client port instead of colliding. Bail if
      // the user removed this queued row in the meantime.
      while (othersBusy()) {
        if (!get().entries.some((e) => e.path === destPath)) return;
        await sleep(400);
      }
      patch({ status: "uploading" });
      // Make sure the console is on the matching (hardened) payload before we
      // stream — same guard the upload queue uses.
      await ensurePayloadCurrent(hostOf(host));
      // Updates/DLC stage into a sub-dir; create it first (mkdir -p,
      // EEXIST-tolerant) so the single-file transfer's open() doesn't fail
      // with ENOENT on a parent that doesn't exist yet.
      if (subdir) {
        try {
          await fsMkdir(transferAddr(host), `${PKG_LIBRARY_DIR}/${subdir}`);
        } catch (e) {
          patch({
            status: "idle",
            bytes: undefined,
            lastResult: {
              ok: false,
              message: `Couldn't create the ${subdir} folder on the PS5: ${pkgError(e)}`,
            },
          });
          return;
        }
      }
      const tx = (await invoke("transfer_file", {
        req: {
          src: localPath,
          dest: destPath,
          addr: transferAddr(host),
          tx_id: null,
        },
      })) as { job_id?: string };
      const jobId = tx.job_id;
      if (!jobId) throw new Error("upload did not start");
      let polls = 0;
      // No-progress watchdog: bail if bytes don't advance for a while.
      // We key on progress rather than a fixed total cap so an honest
      // multi-GB upload isn't killed, while a job wedged in a non-terminal
      // state (or a silently dead transfer) can't spin forever — which
      // would leave the row "uploading" and block every future install.
      const STALL_LIMIT = 240; // × 500ms = 120s with zero progress
      let lastBytes = -1;
      let stalled = 0;
      for (;;) {
        await sleep(500);
        let js: {
          status?: string;
          bytes_sent?: number;
          total_bytes?: number;
          error?: string | null;
        };
        try {
          js = (await invoke("job_status", { jobId })) as typeof js;
        } catch (e) {
          if (++polls >= 5) throw e;
          continue;
        }
        polls = 0;
        if (typeof js.bytes_sent === "number") {
          patch({ bytes: js.bytes_sent, totalBytes: js.total_bytes ?? totalBytes });
          if (js.bytes_sent > lastBytes) {
            lastBytes = js.bytes_sent;
            stalled = 0;
          } else if (++stalled >= STALL_LIMIT) {
            throw new Error("upload stalled — no progress for 2 minutes");
          }
        } else if (++stalled >= STALL_LIMIT) {
          throw new Error("upload stalled — no status from the PS5");
        }
        if (js.status === "done") break;
        if (js.status === "failed") {
          throw new Error(js.error || "upload failed");
        }
      }
      // Settle to idle and re-sync from the dir (authoritative).
      patch({ status: "idle", bytes: undefined });
      await get().refresh(host);
      // Hands-off flow: once the .pkg has landed, kick the install without a
      // second manual click (opt-out via the Install Package screen). install()
      // owns its own waiting/queueing, the FW-12 notice, and — when
      // autoRemoveAfterInstall is on — the post-install cleanup, so "upload →
      // installed → staged copy removed" becomes one action. It never throws
      // (try/finally inside), so awaiting it here is safe; the caller already
      // treats addAndUpload as fire-and-forget.
      if (useInstallSettingsStore.getState().autoInstallAfterUpload) {
        await get().install(destPath, host);
      }
    } catch (e) {
      // Drop the optimistic row and surface the error.
      set({
        entries: get().entries.filter((e2) => e2.path !== destPath),
        error: `Upload failed: ${pkgError(e)}`,
      });
    }
  },

  async install(path, host) {
    if (!host?.trim() || get().installing) return;
    set({ installing: true, busyNotice: null });
    // Any transfer that the payload swap would interrupt: an Upload-screen
    // transfer, or a .pkg upload/queued here. Installing must wait for all of
    // them so it doesn't tear the payload out mid-upload.
    const transfersActive = () =>
      transferScreenBusy(host) ||
      get().entries.some(
        (e) => e.status === "uploading" || e.status === "queued",
      );
    const patch = (p: Partial<PkgEntry>) =>
      set({
        entries: get().entries.map((e) => (e.path === path ? { ...e, ...p } : e)),
      });
    try {
      // Queue behind any active transfer instead of crashing it. Cancellable
      // via cancelPendingInstall (which flips `installing` off mid-wait).
      if (transfersActive()) {
        set({
          busyNotice:
            "Waiting for the current upload to finish before installing…",
        });
        while (transfersActive()) {
          if (!get().installing) return; // user cancelled the pending install
          await sleep(400);
        }
        set({ busyNotice: null });
      }
      // Inside the try so any throw still hits `finally` and clears the
      // `installing` flag — otherwise a wedged flag would lock the screen.
      patch({ status: "installing", lastResult: undefined });

      // FW 12.xx heads-up: the main-payload InstallByPackage hands the install
      // to Sony's installer from the jailbroken context, which briefly
      // destabilizes SceShellUI on newer firmware — the PS5 screen goes black
      // for a few seconds, then recovers with the install queued. It still
      // completes + launches fine (HW-reported), so we DON'T switch to the DPI
      // daemon (that risks an unlaunchable metadata-only install on 12.xx) —
      // we just warn so the blip isn't alarming. Gated to FW >= 12 where it's
      // observed; older firmware installs silently.
      {
        const rt =
          useConnectionStore.getState().runtimeByHost[hostOf(host)] ?? null;
        const fw = parsePS5Firmware(rt?.ps5Kernel ?? null);
        const major = fw ? parseFloat(fw) : 0;
        if (major >= 12) {
          set({
            busyNotice:
              "Installing… on FW 12.x the PS5 screen may go black for a few seconds — that's normal. Don't touch the console; the install finishes in the background.",
          });
        }
      }

      // The library entry carries the content id parsed at upload time —
      // pass it so the engine doesn't need to re-read a PC-side file (the
      // pkg is already staged on the PS5). The install cascade itself lives in
      // the shared `runPkgInstall` helper (also used by the upload queue's pkg
      // finisher), so the mechanism stays identical across both surfaces.
      const entry = get().entries.find((e) => e.path === path);
      const { installed, mayNotLaunch, errMessage: mainErr } =
        await runPkgInstall(host, path, entry?.contentId || null);

      if (installed) {
        patch({ status: "idle", lastResult: installedLastResult(mayNotLaunch) });
        // Optional: auto-delete the spent staged .pkg so the library
        // doesn't accumulate installed packages. Best-effort + awaited so
        // the row vanishes deterministically; remove() swallows its own
        // errors (surfaces via store.error, never throws here).
        if (useInstallSettingsStore.getState().autoRemoveAfterInstall) {
          await get().remove(path, host);
        }
      } else {
        patch({
          status: "idle",
          lastResult: { ok: false, message: mainErr || "Install was rejected." },
        });
      }
    } catch (e) {
      patch({ status: "idle", lastResult: { ok: false, message: pkgError(e) } });
    } finally {
      set({ installing: false, busyNotice: null });
    }
  },

  cancelPendingInstall() {
    // Only abandon an install still WAITING its turn (busyNotice set) — never
    // yank a real install mid-swap, which would leave the payload half-loaded.
    if (get().busyNotice) {
      set({ installing: false, busyNotice: null });
    }
  },

  async remove(path, host) {
    if (!host?.trim()) return;
    // Optimistic removal — drop the row immediately, restore on failure.
    const prev = get().entries;
    set({ entries: prev.filter((e) => e.path !== path), error: null });
    try {
      await fsDelete(mgmtAddr(host), path);
    } catch (e) {
      set({ entries: prev, error: `Delete failed: ${pkgError(e)}` });
    }
  },

  async clearFinished(host) {
    await bulkDelete(get, set, host, isFinishedPkg);
  },

  async clearAll(host) {
    // Only idle rows — never yank a file out from under an in-flight
    // upload/install (status uploading/installing/queued).
    await bulkDelete(get, set, host, (e) => e.status === "idle");
  },
  }));

/**
 * Per-console store registry. One isolated PkgLibrary store instance per PS5
 * host (port-stripped) — each console's library + install state is fully
 * independent and runs in parallel, so an install on one console never blocks
 * another. This is the literal "a separate ps5upload per console" model.
 */
export type PkgLibraryStore = ReturnType<typeof makePkgLibraryStore>;
const pkgLibraryStores = new Map<string, PkgLibraryStore>();

/** Get (creating on first use) the isolated store for one console. */
export function pkgLibraryStore(host: string): PkgLibraryStore {
  const key = hostOf(host) || "_unset_";
  let s = pkgLibraryStores.get(key);
  if (!s) {
    s = makePkgLibraryStore();
    pkgLibraryStores.set(key, s);
  }
  return s;
}

/**
 * Drop the cached store for a console. Call when a roster profile is removed
 * or re-pointed at a new IP, so a later console that REUSES that IP starts
 * clean instead of inheriting the old console's transient state (entries are
 * re-listed from the live PS5 on mount, but `installing` / `lastResult` /
 * `busyNotice` would otherwise carry over). No-op if nothing is cached for the
 * host. The next `pkgLibraryStore(host)` lazily creates a fresh instance.
 */
export function evictPkgLibraryStore(host: string): void {
  pkgLibraryStores.delete(hostOf(host) || "_unset_");
}

/**
 * Hook: subscribe to ONE console's PkgLibrary store. Pass the host so each
 * Install Package view binds to that console's isolated state. (Replaces the
 * old global `usePkgLibrary(selector)` — the host argument is what makes the
 * per-console isolation work.)
 */
export function usePkgLibrary<T>(
  host: string,
  selector: (s: PkgLibraryState) => T,
): T {
  return useStore(pkgLibraryStore(host), selector);
}

/** Shared optimistic bulk-delete for clearFinished/clearAll. Drops the
 *  matching rows immediately, deletes each on the PS5 concurrently, and
 *  restores only the ones whose delete failed (so a partial failure
 *  leaves the list accurate). Pure-ish: takes the store's get/set so it
 *  unit-tests against the store with a mocked fsDelete. */
async function bulkDelete(
  get: () => PkgLibraryState,
  set: (
    partial:
      | Partial<PkgLibraryState>
      | ((s: PkgLibraryState) => Partial<PkgLibraryState>),
  ) => void,
  host: string,
  pred: (e: PkgEntry) => boolean,
): Promise<void> {
  if (!host?.trim()) return;
  const prev = get().entries;
  const targets = prev.filter(pred);
  if (targets.length === 0) return;
  const targetPaths = new Set(targets.map((e) => e.path));
  // Optimistic: drop all targets up front.
  set({ entries: prev.filter((e) => !targetPaths.has(e.path)), error: null });
  const addr = mgmtAddr(host);
  const failed: PkgEntry[] = [];
  await Promise.all(
    targets.map(async (e) => {
      try {
        await fsDelete(addr, e.path);
      } catch {
        failed.push(e);
      }
    }),
  );
  if (failed.length > 0) {
    const failedPaths = new Set(failed.map((e) => e.path));
    // Re-insert the rows we couldn't delete, de-duped against whatever
    // the list looks like now (a concurrent refresh may have re-added
    // some), and surface a single summary error.
    set((s) => ({
      entries: [
        ...s.entries.filter((e) => !failedPaths.has(e.path)),
        ...failed,
      ],
      error: `Failed to delete ${failed.length} file(s).`,
    }));
  }
}
