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
  return encodeURIComponent(s).replace(/[!'()*]/g, (c) =>
    `%${c.charCodeAt(0).toString(16).toUpperCase()}`,
  );
}

/** Build a path + optional `?addr=` query, matching the Rust `addr_url()`. */
function addrUrl(path: string, addr?: string | null): string {
  return addr && addr.length > 0 ? `${path}?addr=${uenc(addr)}` : path;
}

// ── Fetch helpers ─────────────────────────────────────────────────────────────

const TIMEOUT_STANDARD = 60_000;   // 60 s — matches Rust http_client
const TIMEOUT_LONG     = 3_600_000; // 1 h  — matches Rust http_client_long

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
export async function browserInvoke<T>(cmd: string, args: AnyArgs = {}): Promise<T> {
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
      const { addr, path } = args["req"] as { addr?: string | null; path: string };
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
      const { addr, image_path, mount_name, mount_point, read_only } =
        args["req"] as {
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

    case "ps5_syslog_tail":
      return getJson<T>(addrUrl("/api/ps5/syslog/tail", args["addr"]));

    case "ps5_time_get":
      return getJson<T>(addrUrl("/api/ps5/time/get", args["addr"]));

    case "ps5_time_sync":
      // TS caller: { addr, targetUnixSeconds }
      return postJson<T>("/api/ps5/time/sync", {
        addr: args["addr"],
        target_unix_seconds: args["targetUnixSeconds"],
      });

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
      if (args["summerPolicy"] != null) body["summer_policy"] = args["summerPolicy"];
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
      const { addr, uid } = args["req"] as { addr?: string | null; uid: number };
      let url = `/api/profile/avatar/current?uid=${uid}`;
      if (addr) url = `/api/profile/avatar/current?addr=${uenc(addr)}&uid=${uid}`;
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
      const { addr, slot } = args["req"] as { addr?: string | null; slot: number };
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
      const { addr, path } = args as { addr?: string | null; path: string };
      let url = `/api/ps5/pkg/metadata?path=${uenc(path)}`;
      if (addr) url = `/api/ps5/pkg/metadata?addr=${uenc(addr)}&path=${uenc(path)}`;
      return getJson<T>(url);
    }

    case "pkg_install_status":
      return getJson<T>(
        `/api/pkg/install/status?session=${uenc(args["session"] as string)}`,
      );

    case "pkg_install_cancel":
      return postJson<T>("/api/pkg/install/cancel", {
        session: args["session"],
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

    // ── Native-only / unsupported ────────────────────────────────────────────

    default:
      throw new BrowserUnsupportedError(cmd);
  }
}
