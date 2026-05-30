import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

import { fsListDir, fsDelete } from "../api/ps5";
import { hostOf, mgmtAddr, transferAddr } from "../lib/addr";
import { humanizePs5Error } from "../lib/humanizeError";
import { stagingBasename } from "../lib/pkgStagingPath";

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
 * Install always goes through the **DPI daemon** (the only method now):
 * `dpi_ensure` brings up (or reuses) the :9040 daemon, `pkg_dpi_install`
 * runs `sceAppInstUtilAppInstallPkg` from that clean loader process, then we
 * re-send the main payload (the single-payload loader swapped it out).
 *
 * IMPORTANT: the library dir is deliberately NOT `pkg_temp/` — the payload
 * sweeps that on boot (`runtime_sweep_stale_pkg_temp`), which would wipe the
 * user's library. `pkg_library/` is left untouched.
 */
export const PKG_LIBRARY_DIR = "/user/data/ps5upload/pkg_library";

export type PkgStatus = "idle" | "uploading" | "installing";

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
  /** Transient per-row state (never persisted; recomputed each session). */
  status: PkgStatus;
  /** Bytes transferred so far (upload) — drives the row progress bar. */
  bytes?: number;
  /** Total bytes for the active upload. */
  totalBytes?: number;
  /** Outcome of the last install attempt this session, for inline feedback. */
  lastResult?: { ok: boolean; message: string };
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
  head?: { content_id?: string; title?: string; warnings?: string[] };
}

function pkgError(e: unknown): string {
  return humanizePs5Error(e instanceof Error ? e.message : String(e));
}

interface PkgLibraryState {
  /** Library contents, derived from the on-PS5 dir + transient row state. */
  entries: PkgEntry[];
  loading: boolean;
  error: string | null;
  /** True while an install is mid-flight. Installs swap the payload in/out,
   *  so the UI blocks refresh/other installs until it settles. */
  installing: boolean;

  refresh: (host: string) => Promise<void>;
  addAndUpload: (localPath: string, host: string) => Promise<void>;
  install: (path: string, host: string) => Promise<void>;
  remove: (path: string, host: string) => Promise<void>;
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
  return [...active, ...merged].sort((a, b) =>
    (a.title ?? a.contentId ?? a.name)
      .toLowerCase()
      .localeCompare((b.title ?? b.contentId ?? b.name).toLowerCase()),
  );
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

export const usePkgLibrary = create<PkgLibraryState>((set, get) => ({
  entries: [],
  loading: false,
  error: null,
  installing: false,

  async refresh(host) {
    if (!host?.trim() || get().installing) return;
    set({ loading: true, error: null });
    const addr = mgmtAddr(host);
    const titles = loadTitleCache();
    try {
      let listed: Awaited<ReturnType<typeof fsListDir>> = [];
      try {
        listed = await fsListDir(addr, PKG_LIBRARY_DIR);
      } catch {
        // Dir doesn't exist yet (no uploads) → empty library, not an error.
        listed = [];
      }
      const entries: PkgEntry[] = listed
        .filter((e) => e.kind === "file" && e.name.toLowerCase().endsWith(".pkg"))
        .map((e) => {
          const contentId = contentIdFromName(e.name);
          return {
            name: e.name,
            path: `${PKG_LIBRARY_DIR}/${e.name}`,
            size: e.size,
            contentId,
            title: titles[contentId],
            titleId: titleIdFromContentId(contentId) ?? undefined,
            status: "idle" as PkgStatus,
          };
        });
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
    const totalBytes = meta.total_size ?? 0;
    if (title) cacheTitle(contentId, title);

    // 2. Name the on-PS5 file `<ContentID>.pkg` (Sony's installer keys on the
    //    basename matching the ContentID — see lib/pkgStagingPath).
    const basename = stagingBasename(
      contentId,
      Math.random().toString(36).slice(2),
      Date.now(),
    );
    const destPath = `${PKG_LIBRARY_DIR}/${basename}`;

    // 3. Optimistic uploading row.
    const optimistic: PkgEntry = {
      name: basename,
      path: destPath,
      size: totalBytes,
      contentId,
      title,
      titleId: titleIdFromContentId(contentId) ?? undefined,
      status: "uploading",
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
    const ip = hostOf(host);
    set({ installing: true });
    const patch = (p: Partial<PkgEntry>) =>
      set({
        entries: get().entries.map((e) => (e.path === path ? { ...e, ...p } : e)),
      });
    try {
      // Inside the try so any throw still hits `finally` and clears the
      // `installing` flag — otherwise a wedged flag would lock the screen.
      patch({ status: "installing", lastResult: undefined });
      // 1. Ensure the DPI daemon is up (reuses a live one, else sends the
      //    bundled ezremote-dpi.elf to the loader — swapping our payload out).
      const ens = (await invoke("dpi_ensure", { ip })) as {
        ok?: boolean;
        error?: string;
      };
      if (!ens.ok) {
        throw new Error(ens.error || "the DPI daemon didn't come up on :9040");
      }
      // 2. Install the staged pkg via DPI.
      let resp: { ok?: boolean; rc?: number; err_message?: string } = {};
      try {
        resp = (await invoke("pkg_dpi_install", {
          ps5Addr: mgmtAddr(host),
          localPs5Path: path,
        })) as typeof resp;
      } catch (e) {
        resp = { ok: false, rc: 0, err_message: pkgError(e) };
      }
      // 3. Restore our main payload — the daemon replaced it on a
      //    single-payload loader. Best-effort.
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
      if (resp.ok) {
        patch({ status: "idle", lastResult: { ok: true, message: "Installed." } });
      } else {
        const rc = (resp.rc ?? 0) >>> 0;
        patch({
          status: "idle",
          lastResult: {
            ok: false,
            message:
              resp.err_message ||
              `Install was rejected (0x${rc.toString(16).padStart(8, "0")}).`,
          },
        });
      }
    } catch (e) {
      patch({ status: "idle", lastResult: { ok: false, message: pkgError(e) } });
    } finally {
      set({ installing: false });
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
}));
