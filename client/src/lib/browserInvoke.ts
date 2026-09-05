/**
 * Browser transport shim: translates Tauri `invoke(cmd, args)` calls into
 * `fetch()` requests against the ps5upload engine.
 *
 * This file is the sole place that knows the mapping from Tauri command name
 * → HTTP method + path + body. The authoritative spec for each mapping is the
 * corresponding `#[tauri::command]` in `client/src-tauri/src/commands/`:
 *   - `ps5_engine.rs`  — most commands (HTTP proxies to engine routes)
 *   - `probes.rs`      — `payload_check`
 *
 * Commands that are native-only (local filesystem, host temp-zip, OS APIs)
 * throw `BrowserUnsupportedError` so callers fail loudly in dev rather than
 * silently doing nothing. Feature gating (hiding those buttons in the browser
 * UI) is handled by `isTauriEnv()` checks at the call sites.
 */

import { getEngineUrl } from "../state/engine";

// ── Error types ──────────────────────────────────────────────────────────────

/** Thrown by `browserInvoke` for commands that require the Tauri desktop
 *  client and have no browser/HTTP equivalent. Callers that might reach these
 *  paths in non-Tauri builds should guard with `isTauriEnv()`. */
export class BrowserUnsupportedError extends Error {
  readonly cmd: string;
  constructor(cmd: string) {
    super(
      `"${cmd}" requires the Tauri desktop client — it is not available in the browser.` +
        ` Guard the call site with isTauriEnv() or hide the UI affordance.`,
    );
    this.name = "BrowserUnsupportedError";
    this.cmd = cmd;
  }
}

// ── URL helpers ───────────────────────────────────────────────────────────────

/**
 * Percent-encode a string matching the Rust engine's own `urlencoding()`:
 * only ASCII alphanumeric and `-_.~` are left bare; everything else becomes
 * `%XX`.  `encodeURIComponent` is close but leaves `!'()*` unencoded — strip
 * those too so embedded `!` in PS5 paths don't corrupt query strings.
 */
function uenc(s: string): string {
  return encodeURIComponent(s).replace(
    /[!'()*]/g,
    (c) => `%${c.charCodeAt(0).toString(16).toUpperCase()}`,
  );
}

/**
 * Request body for `ps5_time_sync`, mapping the TS callers' camelCase
 * arguments onto the engine's snake_case route.
 *
 * Exported so the mapping is unit-testable: this request sets a games
 * console's system clock, and a key lost in translation does not throw
 * — it quietly changes the time that gets written. `target_unix_seconds`
 * is omitted entirely for an NTP sync rather than sent as 0, which the
 * engine would read as 1970.
 */
export function timeSyncBody(
  args: Record<string, unknown>,
): Record<string, unknown> {
  const useNtp = args["useNtp"] === true;
  const body: Record<string, unknown> = {
    addr: args["addr"],
    use_ntp: useNtp,
  };
  if (useNtp) {
    if (args["ntpServer"] != null) body["ntp_server"] = args["ntpServer"];
  } else {
    body["target_unix_seconds"] = args["targetUnixSeconds"];
  }
  return body;
}

/** Build a path + optional `?addr=` query, matching the Rust `addr_url()`. */
function addrUrl(path: string, addr?: string | null): string {
  return addr && addr.length > 0 ? `${path}?addr=${uenc(addr)}` : path;
}

/**
 * Build `path?k=v&…` using the engine's own encoding, skipping params that
 * are null/undefined/empty-string.
 *
 * This mirrors the `let mut params = Vec::new(); … params.join("&")` shape
 * the Rust commands in `ps5_engine.rs` use, so both transports produce
 * byte-identical URLs. Numeric `0` and boolean `false` are NOT skipped —
 * `since_seq=0` is a meaningful cursor, and dropping it would silently
 * replay the whole notification backlog.
 */
function qs(
  path: string,
  params: Record<string, string | number | boolean | null | undefined>,
): string {
  const parts: string[] = [];
  for (const [k, v] of Object.entries(params)) {
    if (v === null || v === undefined || v === "") continue;
    parts.push(`${k}=${uenc(String(v))}`);
  }
  return parts.length > 0 ? `${path}?${parts.join("&")}` : path;
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

const TIMEOUT_STANDARD = 60_000; // 60 s — matches Rust http_client
const TIMEOUT_LONG = 3_600_000; // 1 h  — matches Rust http_client_long

async function extractEngineError(r: Response): Promise<string> {
  try {
    const j = (await r.json()) as Record<string, unknown>;
    if (j && typeof j["error"] === "string") return j["error"] as string;
  } catch {
    /* fall through */
  }
  const text = await r.text().catch(() => "");
  return text.trim() || `engine HTTP ${r.status}`;
}

async function getJson<T>(path: string): Promise<T> {
  const url = `${getEngineUrl()}${path}`;
  const r = await fetch(url, { signal: AbortSignal.timeout(TIMEOUT_STANDARD) });
  if (!r.ok) throw new Error(await extractEngineError(r));
  return r.json() as Promise<T>;
}

async function postJson<T>(
  path: string,
  body: unknown,
  long = false,
): Promise<T> {
  const url = `${getEngineUrl()}${path}`;
  const r = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    signal: AbortSignal.timeout(long ? TIMEOUT_LONG : TIMEOUT_STANDARD),
  });
  if (!r.ok) throw new Error(await extractEngineError(r));
  return r.json() as Promise<T>;
}

// ── SSE (streaming archive inspect) ──────────────────────────────────────────

/** Matches `INSPECT_IDLE_TIMEOUT_SECS` in `commands/ps5_engine.rs`. A slow
 *  network mount or a spun-down USB HDD can go quiet for a long time between
 *  events; only a truly wedged engine should trip this. */
const INSPECT_IDLE_TIMEOUT_MS = 30_000;

/** The shape `api/ps5.ts` hands us for progress. In Tauri this is a real
 *  `Channel`; in the browser we only ever touch `.onmessage`, so we accept
 *  any object carrying one. */
type ProgressChannel = { onmessage?: (p: unknown) => void };

/**
 * Consume the engine's `text/event-stream` inspect endpoints, forwarding
 * `progress` events to `channel.onmessage` and resolving with the `done`
 * payload — the browser-side twin of `post_sse_inspect_with_watchdog()`.
 *
 * The idle watchdog is the reason this can't just be an `EventSource`:
 * these endpoints are POSTs with a JSON body, and `EventSource` can only
 * issue bodyless GETs. Progress delivery is fire-and-forget to match the
 * Rust side — a throwing callback (e.g. an unmounted component) must not
 * abort an inspect that is otherwise succeeding.
 */
async function postSseInspect<T>(
  path: string,
  body: unknown,
  channel: ProgressChannel | undefined,
): Promise<T> {
  const r = await fetch(`${getEngineUrl()}${path}`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "text/event-stream" },
    body: JSON.stringify(body),
  });
  // A non-200 means the engine rejected the request shape outright; stream
  // errors arrive as a final `event: error` on an otherwise-200 response.
  if (!r.ok) throw new Error(await extractEngineError(r));
  if (!r.body) throw new Error("engine returned no SSE body");

  const reader = r.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  try {
    for (;;) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      const idle = new Promise<never>((_, reject) => {
        timer = setTimeout(
          () =>
            reject(
              new Error(
                `engine stopped responding (no SSE event for ${INSPECT_IDLE_TIMEOUT_MS / 1000}s)`,
              ),
            ),
          INSPECT_IDLE_TIMEOUT_MS,
        );
      });
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await Promise.race([reader.read(), idle]);
      } finally {
        if (timer !== undefined) clearTimeout(timer);
      }
      if (chunk.done) {
        throw new Error(
          "engine closed the inspect stream before sending a done/error event",
        );
      }
      buf += decoder.decode(chunk.value, { stream: true });

      for (;;) {
        const m = /\r?\n\r?\n/.exec(buf);
        if (!m) break;
        const block = buf.slice(0, m.index);
        buf = buf.slice(m.index + m[0].length);
        const ev = parseSseBlock(block);
        if (!ev) continue;
        if (ev.event === "progress") {
          try {
            channel?.onmessage?.(JSON.parse(ev.data));
          } catch {
            /* fire-and-forget, exactly like the Rust Channel::send */
          }
        } else if (ev.event === "done") {
          return JSON.parse(ev.data) as T;
        } else if (ev.event === "error") {
          let msg = ev.data;
          try {
            const j = JSON.parse(ev.data) as Record<string, unknown>;
            if (typeof j["error"] === "string") msg = j["error"] as string;
          } catch {
            /* fall back to the raw data */
          }
          throw new Error(`engine reported: ${msg}`);
        }
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
}

/** Parse one SSE block into `{ event, data }`, mirroring the Rust
 *  `parse_sse_block()` — multi-line `data:` fields join with newlines and
 *  comment lines (`:`) are skipped. Exported for unit tests. */
export function parseSseBlock(
  block: string,
): { event?: string; data: string } | null {
  let event: string | undefined;
  const data: string[] = [];
  for (const raw of block.split("\n")) {
    const line = raw.replace(/\r+$/, "");
    if (line === "" || line.startsWith(":")) continue;
    if (line.startsWith("event:")) event = line.slice(6).replace(/^ +/, "");
    else if (line.startsWith("data:")) data.push(line.slice(5).replace(/^ +/, ""));
  }
  if (event === undefined && data.length === 0) return null;
  return { event, data: data.join("\n") };
}

// ── Dispatch ──────────────────────────────────────────────────────────────────

type AnyArgs = Record<string, any>;

/**
 * Translate a Tauri command name + args into a `fetch()` against the engine.
 * Called by `invokeLogged.ts` when `!isTauriEnv()`.
 *
 * Arg shapes follow the Tauri 2 IPC convention:
 *   - Scalar params: camelCase keys in the args object
 *     (Tauri 2 converts `job_id` → `jobId` on the TS side).
 *   - Struct params (`req: FooReq`): nested as `{ req: { snake_case_fields } }`
 *     (serde default naming is preserved inside the struct).
 */
export async function browserInvoke<T>(
  cmd: string,
  args: AnyArgs = {},
): Promise<T> {
  switch (cmd) {
    // ── PS5 filesystem ──────────────────────────────────────────────────────

    case "ps5_volumes":
      return getJson<T>(addrUrl("/api/ps5/volumes", args["addr"]));

    case "ps5_list_dir": {
      const { addr, path, offset, limit } = args as {
        addr?: string | null;
        path: string;
        offset?: number;
        limit?: number;
      };
      let url = addrUrl("/api/ps5/list-dir", addr);
      url += `${url.includes("?") ? "&" : "?"}path=${uenc(path)}`;
      if (offset !== undefined) url += `&offset=${offset}`;
      if (limit !== undefined) url += `&limit=${limit}`;
      return getJson<T>(url);
    }

    case "ps5_fs_delete": {
      const { addr, path, op_id } = args["req"] as {
        addr?: string | null;
        path: string;
        op_id?: number;
      };
      return postJson<T>(
        "/api/ps5/fs/delete",
        { addr, path, op_id: op_id ?? 0 },
        /*long=*/ true,
      );
    }

    case "ps5_fs_move": {
      const { addr, from, to, op_id } = args["req"] as {
        addr?: string | null;
        from: string;
        to: string;
        op_id?: number;
      };
      return postJson<T>(
        "/api/ps5/fs/move",
        { addr, from, to, op_id: op_id ?? 0 },
        /*long=*/ true,
      );
    }

    case "ps5_fs_copy": {
      const { addr, from, to, op_id } = args["req"] as {
        addr?: string | null;
        from: string;
        to: string;
        op_id?: number;
      };
      return postJson<T>(
        "/api/ps5/fs/copy",
        { addr, from, to, op_id: op_id ?? 0 },
        /*long=*/ true,
      );
    }

    case "ps5_fs_op_status": {
      const { addr, op_id } = args["req"] as {
        addr?: string | null;
        op_id: number;
      };
      let url = `/api/ps5/fs/op-status?op_id=${op_id}`;
      if (addr) url += `&addr=${uenc(addr)}`;
      return getJson<T>(url);
    }

    case "ps5_fs_op_cancel": {
      const { addr, op_id } = args["req"] as {
        addr?: string | null;
        op_id: number;
      };
      return postJson<T>("/api/ps5/fs/op-cancel", { addr, op_id });
    }

    case "ps5_fs_mkdir": {
      const { addr, path } = args["req"] as {
        addr?: string | null;
        path: string;
      };
      return postJson<T>("/api/ps5/fs/mkdir", { addr, path });
    }

    case "ps5_fs_chmod": {
      const { addr, path, mode, recursive } = args["req"] as {
        addr?: string | null;
        path: string;
        mode: string;
        recursive?: boolean;
      };
      return postJson<T>("/api/ps5/fs/chmod", {
        addr,
        path,
        mode,
        recursive: recursive ?? false,
      });
    }

    case "ps5_fs_mount": {
      const { addr, image_path, mount_name, mount_point, read_only } = args[
        "req"
      ] as {
        addr?: string | null;
        image_path: string;
        mount_name?: string | null;
        mount_point?: string | null;
        read_only?: boolean | null;
      };
      return postJson<T>("/api/ps5/fs/mount", {
        addr,
        image_path,
        mount_name,
        mount_point,
        read_only,
      });
    }

    case "ps5_fs_unmount": {
      const { addr, mount_point } = args["req"] as {
        addr?: string | null;
        mount_point: string;
      };
      return postJson<T>("/api/ps5/fs/unmount", { addr, mount_point });
    }

    // ── PS5 apps ────────────────────────────────────────────────────────────

    case "ps5_apps_installed":
      return getJson<T>(addrUrl("/api/ps5/apps/installed", args["addr"]));

    case "ps5_readiness":
      return getJson<T>(addrUrl("/api/ps5/readiness", args["addr"]));

    case "ps5_app_launch": {
      const { addr, title_id } = args["req"] as {
        addr?: string | null;
        title_id: string;
      };
      return postJson<T>("/api/ps5/app/launch", { addr, title_id });
    }

    case "ps5_app_register": {
      const { addr, src_path, patch_drm_type } = args["req"] as {
        addr?: string | null;
        src_path: string;
        patch_drm_type?: boolean | null;
      };
      return postJson<T>("/api/ps5/app/register", {
        addr,
        src_path,
        patch_drm_type,
      });
    }

    case "ps5_app_unregister": {
      const { addr, title_id } = args["req"] as {
        addr?: string | null;
        title_id: string;
      };
      return postJson<T>("/api/ps5/app/unregister", { addr, title_id });
    }

    // ── Hardware monitoring ─────────────────────────────────────────────────

    case "ps5_hw_info":
      return getJson<T>(addrUrl("/api/ps5/hw/info", args["addr"]));

    case "ps5_hw_temps": {
      const { addr, extended } = args as {
        addr?: string | null;
        extended?: boolean | null;
      };
      let url = addrUrl("/api/ps5/hw/temps", addr);
      if (extended) url += `${url.includes("?") ? "&" : "?"}extended=1`;
      return getJson<T>(url);
    }

    case "ps5_hw_power":
      return getJson<T>(addrUrl("/api/ps5/hw/power", args["addr"]));

    case "ps5_hw_storage":
      return getJson<T>(addrUrl("/api/ps5/hw/storage", args["addr"]));

    case "ps5_hw_set_fan_threshold":
      // TS caller: { addr, thresholdC } — engine body uses threshold_c
      return postJson<T>("/api/ps5/hw/fan-threshold", {
        addr: args["addr"],
        threshold_c: args["thresholdC"],
      });

    // ── Syslog + time ───────────────────────────────────────────────────────

    // Reads a console file for the bug-report collector. Without this the
    // browser build silently collected NO on-console logs: the collector
    // (correctly) treats a per-file read failure as "file absent", so the
    // missing route looked like an empty console rather than an error.
    case "fs_read_preview":
      return postJson<T>("/api/ps5/fs/read-preview", {
        addr: args["addr"],
        path: args["path"],
        max_bytes: args["maxBytes"] ?? args["max_bytes"],
      });

    // App lifecycle (suspend / resume / kill / list). Without these the
    // browser build could not close a running game at all — the desktop app
    // reaches them through Tauri commands that call core directly, with no
    // HTTP route behind them.
    case "app_suspend":
    case "app_resume":
    case "app_kill":
    case "app_list_running": {
      const action =
        cmd === "app_suspend"
          ? "suspend"
          : cmd === "app_resume"
            ? "resume"
            : cmd === "app_kill"
              ? "kill"
              : "list";
      return postJson<T>("/api/ps5/app/lifecycle", {
        addr: args["addr"],
        action,
        app_id: args["appId"] ?? args["app_id"] ?? 0,
      });
    }

    // Console diagnostics used by the bug-report collector. Without these
    // a web-generated report silently carried no kernel log, no network
    // interfaces and no process list — the collector treats an unsupported
    // command as "nothing to collect", so the gap looked like a quiet
    // console rather than a broken route.
    case "klog_chunk": {
      let url = addrUrl("/api/ps5/klog", args["addr"]);
      const mb = args["maxBytes"] ?? args["max_bytes"];
      if (mb) url += `${url.includes("?") ? "&" : "?"}max_bytes=${mb}`;
      // The desktop command resolves to the log text itself, not an object.
      return getJson<{ text?: string }>(url).then(
        (r) => (r?.text ?? "") as unknown as T,
      );
    }

    case "net_interfaces_get":
      return getJson<T>(addrUrl("/api/ps5/net/interfaces", args["addr"]));

    case "proc_list_get":
      return getJson<T>(addrUrl("/api/ps5/proc/list", args["addr"]));

    case "remoteplay_status":
      return getJson<T>(addrUrl("/api/ps5/remoteplay/status", args["addr"]));

    case "remoteplay_cancel":
      return postJson<T>("/api/ps5/remoteplay/cancel", { addr: args["addr"] });

    case "remoteplay_request": {
      // TS caller nests everything under `req`.
      const req = (args["req"] ?? {}) as Record<string, unknown>;
      return postJson<T>("/api/ps5/remoteplay/request", {
        addr: req["addr"] ?? args["addr"],
        manual_account_id: req["manual_account_id"] ?? null,
      });
    }

    case "ps5_focus":
      return getJson<T>(addrUrl("/api/ps5/focus", args["addr"]));

    // In the browser the direct <img> URL already works (same origin as the
    // engine), so these only exist to keep the code path uniform.
    case "ps5_app_icon_data":
    case "ps5_game_icon_data": {
      const base = addrUrl(
        cmd === "ps5_app_icon_data"
          ? "/api/ps5/app-icon"
          : "/api/ps5/game-icon",
        args["addr"],
      );
      const sep = base.includes("?") ? "&" : "?";
      const q =
        cmd === "ps5_app_icon_data"
          ? `title_id=${encodeURIComponent(String(args["titleId"] ?? ""))}`
          : `path=${encodeURIComponent(String(args["path"] ?? ""))}`;
      const res = await fetch(`${base}${sep}${q}`);
      if (!res.ok) throw new Error(`icon HTTP ${res.status}`);
      const blob = await res.blob();
      return (await new Promise<string>((resolve, reject) => {
        const fr = new FileReader();
        fr.onload = () => resolve(String(fr.result));
        fr.onerror = () => reject(new Error("icon read failed"));
        fr.readAsDataURL(blob);
      })) as T;
    }

    case "cache_artwork_stats":
      return getJson<T>("/api/cache/artwork");

    case "cache_artwork_clear": {
      const res = await fetch(`${getEngineUrl()}/api/cache/artwork`, {
        method: "DELETE",
      });
      if (!res.ok) throw new Error(`cache clear HTTP ${res.status}`);
      return (await res.json()) as T;
    }

    case "ps5_appinfo_query": {
      const base = addrUrl("/api/ps5/appinfo", args["addr"]);
      const sep = base.includes("?") ? "&" : "?";
      const keys = String(args["keys"] ?? "");
      const q =
        `title_id=${encodeURIComponent(String(args["title_id"] ?? ""))}` +
        (keys ? `&keys=${encodeURIComponent(keys)}` : "");
      return getJson<T>(`${base}${sep}${q}`);
    }

    case "ps5_appinfo_set":
      return postJson<T>("/api/ps5/appinfo/set", {
        addr: args["addr"],
        title_id: args["title_id"],
        key: args["key"],
        val: args["val"],
        backup_dir: args["backup_dir"] ?? null,
      });

    case "ps5_syslog_tail":
      return getJson<T>(addrUrl("/api/ps5/syslog/tail", args["addr"]));

    case "ps5_time_get":
      return getJson<T>(addrUrl("/api/ps5/time/get", args["addr"]));

    case "ps5_time_sync":
      return postJson<T>("/api/ps5/time/sync", timeSyncBody(args));

    case "ps5_time_state_get":
      return getJson<T>(addrUrl("/api/ps5/time/state/get", args["addr"]));

    case "ps5_time_state_set": {
      // TS caller: { addr, tzIndex, dateFormat, timeFormat, summerPolicy, setAuto }
      // Engine expects snake_case, only non-null fields forwarded (belt+suspenders)
      const body: Record<string, unknown> = {};
      if (args["addr"] != null) body["addr"] = args["addr"];
      if (args["tzIndex"] != null) body["tz_index"] = args["tzIndex"];
      if (args["dateFormat"] != null) body["date_format"] = args["dateFormat"];
      if (args["timeFormat"] != null) body["time_format"] = args["timeFormat"];
      if (args["summerPolicy"] != null)
        body["summer_policy"] = args["summerPolicy"];
      if (args["setAuto"] != null) body["set_auto"] = args["setAuto"];
      return postJson<T>("/api/ps5/time/state/set", body);
    }

    // ── SMP meta ────────────────────────────────────────────────────────────

    case "ps5_smp_meta_control": {
      const body: Record<string, unknown> = { action: args["action"] };
      if (args["addr"] != null) body["addr"] = args["addr"];
      if (args["interval"] != null) body["interval"] = args["interval"];
      return postJson<T>("/api/ps5/smp-meta/control", body);
    }

    case "ps5_smp_meta_stats":
      return getJson<T>(addrUrl("/api/ps5/smp-meta/stats", args["addr"]));

    // ── Profile ─────────────────────────────────────────────────────────────

    case "profile_info":
      return getJson<T>(addrUrl("/api/profile/info", args["req"]?.addr));

    case "profile_avatar_current": {
      const { addr, uid } = args["req"] as {
        addr?: string | null;
        uid: number;
      };
      let url = `/api/profile/avatar/current?uid=${uid}`;
      if (addr)
        url = `/api/profile/avatar/current?addr=${uenc(addr)}&uid=${uid}`;
      return getJson<T>(url);
    }

    case "profile_set_username": {
      const { addr, slot, name } = args["req"] as {
        addr?: string | null;
        slot: number;
        name: string;
      };
      return postJson<T>("/api/profile/username", { addr, slot, name });
    }

    case "profile_rename_user": {
      const { addr, uid, name } = args["req"] as {
        addr?: string | null;
        uid: number;
        name: string;
      };
      return postJson<T>("/api/profile/local-username", { addr, uid, name });
    }

    case "profile_activate": {
      const { addr, slot, id } = args["req"] as {
        addr?: string | null;
        slot: number;
        id?: number | null;
      };
      return postJson<T>("/api/profile/activate", { addr, slot, id });
    }

    case "profile_clear_slot": {
      const { addr, slot } = args["req"] as {
        addr?: string | null;
        slot: number;
      };
      return postJson<T>("/api/profile/clear-slot", { addr, slot });
    }

    // ── Jobs ────────────────────────────────────────────────────────────────

    case "job_status": {
      // TS caller: { jobId } (Tauri 2 camelCases job_id → jobId)
      const jobId = args["jobId"] as string;
      return getJson<T>(`/api/jobs/${encodeURIComponent(jobId)}`);
    }

    case "job_cancel": {
      const jobId = args["jobId"] as string;
      return postJson<T>(`/api/jobs/${encodeURIComponent(jobId)}/cancel`, {});
    }

    // ── Engine logs ─────────────────────────────────────────────────────────

    case "engine_logs_tail":
      return getJson<T>(`/api/engine-logs?since=${args["since"] ?? 0}`);

    // ── Packages ────────────────────────────────────────────────────────────

    case "pkg_scan_external":
      return getJson<T>(addrUrl("/api/ps5/pkg/scan-external", args["addr"]));

    case "pkg_metadata_console": {
      const { addr, path, size } = args as {
        addr?: string | null;
        path: string;
        size?: number | null;
      };
      let url = `/api/ps5/pkg/metadata?path=${uenc(path)}`;
      if (addr)
        url = `/api/ps5/pkg/metadata?addr=${uenc(addr)}&path=${uenc(path)}`;
      if (size && size > 0) url += `&size=${size}`;
      return getJson<T>(url);
    }

    case "pkg_install_start":
      return postJson<T>("/api/pkg/install/start", {
        ps5_addr: args["ps5Addr"],
        path: args["path"],
        split_root: args["splitRoot"],
        package_type_override: args["packageTypeOverride"],
        local_ps5_path: args["localPs5Path"],
        content_id: args["contentId"],
        expected_size: args["expectedSize"],
        package_fingerprint: args["packageFingerprint"],
        delete_staging: args["deleteStaging"] ?? true,
        serve_only: args["serveOnly"] ?? false,
      });

    case "pkg_installed_inventory": {
      const addr = args["addr"] as string;
      const titleId = args["titleId"] as string;
      return getJson<T>(
        `/api/pkg/installed?addr=${uenc(addr)}&title_id=${uenc(titleId)}`,
      );
    }

    case "pkg_install_status":
      return getJson<T>(
        `/api/pkg/install/status?session=${uenc(args["session"] as string)}`,
      );

    case "pkg_install_cancel":
      return postJson<T>("/api/pkg/install/cancel", {
        session: args["session"],
      });

    // Bring up the DPI install daemon on :9040, and put the ps5upload
    // helper back once the install is done. On the desktop both of these
    // stream an ELF the app has embedded; a browser has no socket and no
    // copy of the bytes, so the engine does it. Missing here, the install
    // cascade's DPI fallback was unreachable from the web UI — and that
    // fallback is the only path that lands a game PATCH, which is why
    // base games installed from the browser and updates did not (#152).
    case "dpi_ensure":
      // TS caller: { ip }. Response shape matches the Tauri command's
      // { ok, listening, sent, error? } — the cascade reads `sent` to
      // decide whether the helper needs restoring.
      return postJson<T>("/api/pkg/dpi-ensure", {
        ps5_addr: args["ip"],
      });

    case "payload_restore":
      // Browser-only command (no Tauri twin): the desktop resolves its
      // bundled payload path and calls `payload_send`, which a browser
      // cannot do. Never long: a stuck restore must not hold the install
      // cascade's `finally` open for an hour.
      return postJson<T>("/api/pkg/payload-restore", {
        ps5_addr: args["ip"],
      });

    case "pkg_dpi_install":
      // TS caller: { ps5Addr, localPs5Path } (Tauri 2 camelCase)
      return postJson<T>(
        "/api/pkg/dpi-install",
        {
          ps5_addr: args["ps5Addr"],
          local_ps5_path: args["localPs5Path"],
        },
        /*long=*/ true,
      );

    // Local .pkg header/split parse. Missing here meant the browser build
    // could not stream-install at all: installStream reads metadata through
    // this before it can open a serve-only session.
    case "pkg_metadata_split":
      return postJson<T>("/api/pkg/parse-split", { path: args["path"] });

    case "pkg_dpi_direct_install":
      // TS caller: { ps5Addr, sessionId } (Tauri 2 camelCase).
      // Direct/streaming install (beta, #81): the engine serves the pkg
      // at /pkg-host/{session}/ and the DPI daemon pulls it over HTTP —
      // no staging copy uploaded to the PS5 first.
      return postJson<T>(
        "/api/pkg/dpi-direct-install",
        {
          ps5_addr: args["ps5Addr"],
          session_id: args["sessionId"],
        },
        /*long=*/ true,
      );

    // ── Payload probe ────────────────────────────────────────────────────────

    case "payload_check": {
      // Rust probes.rs: GET /api/ps5/status?addr={ip}:9114, wraps the
      // response as { ok, reachable, status } or { ok:false, reachable:false, error }.
      const ip = args["ip"] as string;
      const addr = uenc(`${ip}:9114`);
      const url = `${getEngineUrl()}/api/ps5/status?addr=${addr}`;
      try {
        const r = await fetch(url, { signal: AbortSignal.timeout(5_000) });
        if (r.ok) {
          const status = (await r.json()) as unknown;
          return { ok: true, reachable: true, status } as unknown as T;
        }
        const body = await r.text().catch(() => "");
        return {
          ok: false,
          reachable: false,
          error: body.trim() || `engine HTTP ${r.status}`,
        } as unknown as T;
      } catch (e) {
        return {
          ok: false,
          reachable: false,
          error: e instanceof Error ? e.message : String(e),
        } as unknown as T;
      }
    }

    // ── Process manager ─────────────────────────────────────────────────────

    case "process_list_get":
      return getJson<T>(addrUrl("/api/ps5/process/list", args["addr"]));

    case "process_kill_pid": {
      const { addr, pid } = args as { addr?: string | null; pid: number };
      return postJson<T>("/api/ps5/process/kill", { addr, pid });
    }

    // ── Power control + telemetry ───────────────────────────────────────────

    case "power_reboot":
      return postJson<T>("/api/ps5/power/control", {
        addr: args["addr"],
        action: "reboot",
      });

    case "power_shutdown":
      return postJson<T>("/api/ps5/power/control", {
        addr: args["addr"],
        action: "shutdown",
      });

    case "power_standby":
      return postJson<T>("/api/ps5/power/control", {
        addr: args["addr"],
        action: "standby",
      });

    case "power_tick":
      return postJson<T>("/api/ps5/power/control", {
        addr: args["addr"],
        action: "tick",
      });

    case "power_telemetry_get":
      return getJson<T>(addrUrl("/api/ps5/power/telemetry", args["addr"]));

    // ── User accounts ────────────────────────────────────────────────────────

    case "user_list_get":
      return getJson<T>(addrUrl("/api/ps5/users/list", args["addr"]));

    // ── Saves / screenshots / videos ────────────────────────────────────────

    case "saves_list": {
      // TS caller: { addr, userId } (Tauri 2 camelCases user_id → userId)
      const { addr, userId } = args as {
        addr?: string | null;
        userId?: number | null;
      };
      let url = addrUrl("/api/ps5/saves/list", addr);
      if (userId != null) {
        url += `${url.includes("?") ? "&" : "?"}user_id=${userId}`;
      }
      return getJson<T>(url);
    }

    case "screenshots_list":
      return getJson<T>(addrUrl("/api/ps5/screenshots/list", args["addr"]));

    case "videos_list":
      return getJson<T>(addrUrl("/api/ps5/videos/list", args["addr"]));

    // ── ShadowMount+ status ──────────────────────────────────────────────────

    case "smp_status":
      return getJson<T>(addrUrl("/api/ps5/smp/status", args["addr"]));

    // Small-file write. The desktop app has this as a Tauri command; without
    // the browser counterpart anything that writes a config file to the
    // console failed here — including handing a game to ShadowMount+, which
    // appends to its manual.lst.
    case "fs_write_bytes_run":
      return postJson<T>("/api/ps5/fs/write-bytes", {
        addr: args["addr"],
        path: args["path"],
        bytes_b64: args["bytesB64"],
        create_only: args["createOnly"] ?? false,
      });

    // ── ShadowMount+ edit sessions ──────────────────────────────────────────
    // The engine wraps these in `{ checkout: ... }` so the GET can express
    // "nothing is checked out" as a null field rather than a 404; unwrap here
    // so browser mode and Tauri mode hand the caller the same shape.

    case "smp_checkout_status":
      return getJson<{ checkout: T }>(
        addrUrl("/api/ps5/smp/checkout", args["addr"]),
      ).then((r) => r.checkout);

    case "smp_checkout_begin":
      return postJson<T>("/api/ps5/smp/checkout/begin", {
        addr: args["addr"],
        image_path: args["imagePath"],
        mount_point: args["mountPoint"],
        title_id: args["titleId"] ?? "",
      });

    case "smp_checkout_finish":
      return postJson<{ checkout: T }>("/api/ps5/smp/checkout/finish", {
        addr: args["addr"],
      }).then((r) => r.checkout);

    // ── Local (engine host) filesystem browse ───────────────────────────────
    // Browser-mode counterpart to the Tauri file/folder dialog: browses the
    // ENGINE's own filesystem (e.g. a Docker container's mounted volumes),
    // backing the same in-app picker (LocalPathPicker) Android already uses.

    case "local_list_dir":
      return getJson<T>(
        `/api/local/list-dir?path=${uenc(args["path"] as string)}`,
      );

    case "local_storage_roots":
      return getJson<T>("/api/local/storage-roots");

    case "storage_access_granted":
      // No scoped-storage concept in a browser — always granted, matching
      // the Tauri command's own non-Android ("desktop") behavior.
      return true as unknown as T;

    case "request_storage_access":
      // No permission to request in a browser — no-op, matching the Tauri
      // command's own non-Android behavior.
      return undefined as unknown as T;

    // ── Transfer jobs (Upload screen) ───────────────────────────────────────
    // These take a plain local path the ENGINE reads via std::fs — same
    // contract whether that's the Tauri desktop's bundled engine or a
    // Dockerized one. The TS wrapper's `req` already carries the exact
    // snake_case body the engine handler deserializes, so it's forwarded
    // verbatim (no field remapping, unlike some older hand-written cases).

    case "transfer_file":
      return postJson<T>("/api/transfer/file", args["req"], /*long=*/ true);

    case "transfer_dir":
      return postJson<T>("/api/transfer/dir", args["req"], /*long=*/ true);

    case "transfer_dir_reconcile":
      return postJson<T>(
        "/api/transfer/dir-reconcile",
        args["req"],
        /*long=*/ true,
      );

    // Host-filesystem helpers. The desktop app answers these in-process
    // via Tauri commands; in the browser the "host" is the engine's own
    // machine, which is exactly what the Upload screen browses there.
    // Without these, picking a folder threw BrowserUnsupportedError (#262).
    case "path_kind":
      return getJson<T>(`/api/local/path-kind?path=${uenc(args["path"] as string)}`);

    case "local_image_attach":
      return postJson<T>("/api/local/image/attach", { path: args["path"] });

    case "local_image_detach":
      return postJson<T>("/api/local/image/detach", { device: args["device"] });

    case "local_image_status":
      return getJson<T>("/api/local/image/status");

    case "activity_reset":
      return postJson<T>(
        addrUrl("/api/ps5/activity/reset", args["addr"] as string | null),
        {},
      );

    case "notif_clear":
      return postJson<T>(
        addrUrl("/api/ps5/notif/clear", args["addr"] as string | null),
        {},
      );

    case "health_scan":
      return getJson<T>(
        addrUrl("/api/ps5/health/scan", args["addr"] as string | null),
      );

    case "health_junk":
      return getJson<T>(
        addrUrl("/api/ps5/health/junk", args["addr"] as string | null),
      );

    case "health_fix":
      return postJson<T>("/api/ps5/health/fix", {
        addr: args["addr"],
        action: args["action"],
      });

    case "remoteplay_readiness":
      return getJson<T>(
        addrUrl("/api/ps5/remoteplay/readiness", args["addr"] as string | null),
      );

    case "remoteplay_devices":
      return getJson<T>(
        addrUrl("/api/ps5/remoteplay/devices", args["addr"] as string | null),
      );

    case "remoteplay_enable":
      return postJson<T>(
        addrUrl("/api/ps5/remoteplay/enable", args["addr"] as string | null),
        { scope: args["scope"] },
      );

    case "inspect_folder":
      return getJson<T>(
        `/api/local/inspect-folder?path=${uenc(args["path"] as string)}`,
      );

    // ── Cheats ───────────────────────────────────────────────────────────────
    // The whole Cheats screen used to be dead in a self-hosted browser
    // session: the engine served every one of these routes, but the shim
    // had no mapping so each button threw BrowserUnsupportedError (#300).

    case "cheats_list":
      return getJson<T>(
        qs("/api/ps5/cheats/list", { addr: args["req"]?.addr }),
      );

    case "cheats_get":
      return getJson<T>(
        qs("/api/ps5/cheats/get", {
          addr: args["req"]?.addr,
          title_id: args["req"]?.title_id,
        }),
      );

    case "cheats_delete":
      return getJson<T>(
        qs("/api/ps5/cheats/delete", {
          addr: args["req"]?.addr,
          title_id: args["req"]?.title_id,
        }),
      );

    case "cheats_reload":
      return getJson<T>(
        qs("/api/ps5/cheats/reload", { addr: args["req"]?.addr }),
      );

    case "cheats_status":
      return getJson<T>(
        qs("/api/ps5/cheats/status", { addr: args["req"]?.addr }),
      );

    case "cheats_toggle":
      return postJson<T>("/api/ps5/cheats/toggle", {
        addr: args["req"]?.addr,
        title_id: args["req"]?.title_id,
        index: args["req"]?.index,
        on: args["req"]?.on,
      });

    case "cheats_engine_set":
      return postJson<T>("/api/ps5/cheats/engine-set", {
        addr: args["req"]?.addr,
        enabled: args["req"]?.enabled,
      });

    case "cheats_repos_list":
      return getJson<T>("/api/ps5/cheats/repos/list");

    case "cheats_repos_search":
      return getJson<T>(
        qs("/api/ps5/cheats/repos/search", { query: args["req"]?.query }),
      );

    case "cheats_repos_download":
      return postJson<T>("/api/ps5/cheats/repos/download", {
        addr: args["req"]?.addr,
        repo_id: args["req"]?.repo_id,
        filename: args["req"]?.filename,
        title_id: args["req"]?.title_id,
      });

    // ── Activity / notifications / hardware ──────────────────────────────────

    case "activity_get":
      return getJson<T>(
        qs("/api/ps5/activity/get", { addr: args["req"]?.addr }),
      );

    case "activity_db_query":
      return getJson<T>(
        qs("/api/ps5/activity/db-query", {
          addr: args["req"]?.addr,
          query: args["req"]?.query,
        }),
      );

    case "notif_list":
      return getJson<T>(
        qs("/api/ps5/notif/list", {
          addr: args["req"]?.addr,
          since_seq: args["req"]?.since_seq ?? 0,
        }),
      );

    case "ps5_hw_drive_sensors":
      return getJson<T>(
        addrUrl("/api/ps5/hw/drive-sensors", args["addr"] as string | null),
      );

    case "fan_curve_get":
      return getJson<T>(
        qs("/api/ps5/hw/fan-curve/get", { addr: args["req"]?.addr }),
      );

    case "fan_curve_set":
      return postJson<T>("/api/ps5/hw/fan-curve", {
        addr: args["req"]?.addr,
        points: args["req"]?.points,
      });

    // ── FTP / firmware spoof / SDK changer ───────────────────────────────────

    case "ftp_start":
      return postJson<T>("/api/ps5/ftp/start", {
        addr: args["req"]?.addr,
        port: args["req"]?.port,
        root: args["req"]?.root,
        readonly: args["req"]?.readonly,
        user: args["req"]?.user,
        pass: args["req"]?.pass,
      });

    case "ftp_status":
      return getJson<T>(qs("/api/ps5/ftp/status", { addr: args["req"]?.addr }));

    case "fw_spoof_status":
      return getJson<T>(
        qs("/api/ps5/fw-spoof/status", { addr: args["req"]?.addr }),
      );

    case "sdk_scan":
      return getJson<T>(qs("/api/ps5/sdk/scan", { addr: args["req"]?.addr }));

    case "sdk_patch":
      return postJson<T>("/api/ps5/sdk/patch", {
        addr: args["req"]?.addr,
        title_id: args["req"]?.title_id,
        target_sdk: args["req"]?.target_sdk,
      });

    case "sdk_restore":
      return postJson<T>("/api/ps5/sdk/restore", {
        addr: args["req"]?.addr,
        title_id: args["req"]?.title_id,
      });

    case "tmdb_fetch":
      return getJson<T>(
        qs("/api/ps5/tmdb/fetch", {
          addr: args["req"]?.addr,
          title_id: args["req"]?.title_id,
          // The Rust side pushes `refresh=true` only when set; a literal
          // `refresh=false` would be truthy to the engine's string parse.
          refresh: args["req"]?.refresh ? "true" : undefined,
          region: args["req"]?.region,
        }),
      );

    // ── Users / backup ───────────────────────────────────────────────────────

    case "user_create":
      return postJson<T>("/api/ps5/users/create", {
        addr: args["req"]?.addr,
        name: args["req"]?.name,
      });

    case "user_delete":
      return postJson<T>("/api/ps5/users/delete", {
        addr: args["req"]?.addr,
        uid: args["req"]?.uid,
        wipe_saves: args["req"]?.wipe_saves,
      });

    case "backup_list":
      return getJson<T>(
        qs("/api/ps5/backup/list", {
          addr: args["req"]?.addr,
          tag: args["req"]?.tag,
        }),
      );

    case "backup_snapshot":
      return postJson<T>(
        "/api/ps5/backup/snapshot",
        {
          addr: args["req"]?.addr,
          tag: args["req"]?.tag,
          path: args["req"]?.path,
        },
        /*long=*/ true,
      );

    case "backup_restore":
      return postJson<T>(
        "/api/ps5/backup/restore",
        {
          addr: args["req"]?.addr,
          tag: args["req"]?.tag,
          timestamp: args["req"]?.timestamp,
        },
        /*long=*/ true,
      );

    case "backup_delete":
      return postJson<T>(
        "/api/ps5/backup/delete",
        {
          addr: args["req"]?.addr,
          tag: args["req"]?.tag,
          timestamp: args["req"]?.timestamp,
        },
        /*long=*/ true,
      );

    // ── Package / archive inspection ─────────────────────────────────────────

    case "pkg_metadata":
      return postJson<T>("/api/pkg/parse", { path: args["path"] });

    case "ffpkg_inspect":
      return postJson<T>("/api/ffpkg/inspect", { path: args["path"] });

    case "ffpkg_extract":
      return postJson<T>("/api/ffpkg/extract", {
        ffpkg_path: args["ffpkgPath"],
        inner_path: args["innerPath"],
        dest_dir: args["destDir"],
      });

    case "zip_inspect":
      return postJson<T>(
        "/api/zip/inspect",
        { zip_path: args["req"]?.zip_path },
        /*long=*/ true,
      );

    case "sevenz_inspect":
      return postJson<T>(
        "/api/7z/inspect",
        { archive_path: args["req"]?.archive_path },
        /*long=*/ true,
      );

    case "rar_inspect":
      return postJson<T>(
        "/api/rar/inspect",
        {
          archive_path: args["req"]?.archive_path,
          password: args["req"]?.password,
        },
        /*long=*/ true,
      );

    case "zip_inspect_stream":
      return postSseInspect<T>(
        "/api/zip/inspect/stream",
        { zip_path: args["req"]?.zip_path },
        args["onProgress"] as ProgressChannel | undefined,
      );

    case "sevenz_inspect_stream":
      return postSseInspect<T>(
        "/api/7z/inspect/stream",
        { archive_path: args["req"]?.archive_path },
        args["onProgress"] as ProgressChannel | undefined,
      );

    case "bps_inspect":
      return postJson<T>("/api/bps/inspect", {
        patch_path: args["req"]?.patch_path,
      });

    case "bps_apply":
      return postJson<T>(
        "/api/bps/apply",
        {
          source_path: args["req"]?.source_path,
          patch_path: args["req"]?.patch_path,
          dest_path: args["req"]?.dest_path,
        },
        /*long=*/ true,
      );

    // ── Profile avatar ───────────────────────────────────────────────────────

    case "profile_avatar_preview":
      return postJson<T>("/api/profile/avatar/preview", {
        image_path: args["req"]?.image_path,
        mode: args["req"]?.mode,
      });

    case "profile_apply_avatar":
      return postJson<T>(
        "/api/profile/avatar",
        {
          addr: args["req"]?.addr,
          image_path: args["req"]?.image_path,
          mode: args["req"]?.mode,
          uid: args["req"]?.uid,
          username: args["req"]?.username,
        },
        /*long=*/ true,
      );

    // ── SMB ──────────────────────────────────────────────────────────────────
    // `smb_download_file` is deliberately absent: it writes the fetched bytes
    // to a host path via `resolve_save_dest()`, which has no browser meaning.

    case "smb_list_shares":
      return postJson<T>("/api/smb/list-shares", {
        server: args["req"]?.server,
        user: args["req"]?.user,
        password: args["req"]?.password,
      });

    case "smb_list_dir":
      return postJson<T>("/api/smb/list-dir", {
        server: args["req"]?.server,
        user: args["req"]?.user,
        password: args["req"]?.password,
        share: args["req"]?.share,
        path: args["req"]?.path,
      });

    case "smb_transfer":
      return postJson<T>("/api/smb/transfer", {
        server: args["req"]?.server,
        user: args["req"]?.user,
        password: args["req"]?.password,
        share: args["req"]?.share,
        path: args["req"]?.path,
        dest_root: args["req"]?.dest_root,
        addr: args["req"]?.addr,
        bandwidth_cap_mbps: args["req"]?.bandwidth_cap_mbps,
      });

    // ── Transfers ────────────────────────────────────────────────────────────
    // Paths here are the ENGINE's filesystem, not the browser's. On a
    // self-hosted engine that is the point: the server holds the library and
    // the browser is only the remote control.

    case "transfer_zip":
      return postJson<T>(
        "/api/transfer/zip",
        {
          zip_path: args["req"]?.zip_path,
          dest_root: args["req"]?.dest_root,
          addr: args["req"]?.addr,
          tx_id: args["req"]?.tx_id,
          excludes: args["req"]?.excludes,
          bandwidth_cap_mbps: args["req"]?.bandwidth_cap_mbps,
        },
        /*long=*/ true,
      );

    case "transfer_7z":
      return postJson<T>(
        "/api/transfer/7z",
        {
          archive_path: args["req"]?.archive_path,
          dest_root: args["req"]?.dest_root,
          addr: args["req"]?.addr,
          tx_id: args["req"]?.tx_id,
          excludes: args["req"]?.excludes,
          bandwidth_cap_mbps: args["req"]?.bandwidth_cap_mbps,
        },
        /*long=*/ true,
      );

    case "transfer_rar":
      return postJson<T>(
        "/api/transfer/rar",
        {
          archive_path: args["req"]?.archive_path,
          dest_root: args["req"]?.dest_root,
          addr: args["req"]?.addr,
          tx_id: args["req"]?.tx_id,
          excludes: args["req"]?.excludes,
          bandwidth_cap_mbps: args["req"]?.bandwidth_cap_mbps,
          password: args["req"]?.password,
        },
        /*long=*/ true,
      );

    case "transfer_download":
      return postJson<T>("/api/transfer/download", {
        src_path: args["req"]?.src_path,
        dest_dir: args["req"]?.dest_dir,
        addr: args["req"]?.addr,
        kind: args["req"]?.kind,
        unsafe_read: args["req"]?.unsafe_read,
      });

    case "transfer_download_zip":
      return postJson<T>("/api/transfer/download-zip", {
        src_path: args["req"]?.src_path,
        dest_zip: args["req"]?.dest_zip,
        addr: args["req"]?.addr,
        kind: args["req"]?.kind,
        unsafe_read: args["req"]?.unsafe_read,
      });

    case "transfer_dir_diff_preview":
      return postJson<T>("/api/transfer/dir-diff-preview", {
        src_dir: args["srcDir"],
        dest_root: args["destRoot"],
        addr: args["addr"],
        excludes: args["excludes"],
      });

    // ── Reachability ─────────────────────────────────────────────────────────

    case "port_check":
      // The desktop client opens the socket itself; a browser borrows the
      // engine's, which is on the console's LAN even when the browser is not.
      return getJson<T>(
        qs("/api/ps5/port-check", { ip: args["ip"], port: args["port"] }),
      );

    // ── Native-only / unsupported ────────────────────────────────────────────

    default:
      throw new BrowserUnsupportedError(cmd);
  }
}
