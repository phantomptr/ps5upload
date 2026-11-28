/**
 * PS5Upload Play on a PS5 from PS5Upload (engine play_app_install.rs): what is installed, what
 * this PS5Upload carries, and install / update / remove / open. The app is put on the PS5 the
 * same way the other homebrew app installer does it, so updates work whichever one installed it.
 */
import { getEngineUrl } from "../../state/engine";

export type PlayAppAction = "install" | "update" | "none" | "newer_installed" | "unavailable";

export interface PlayAppStatus {
  /** The copy this PS5Upload carries; null when this build has none. */
  bundled: { version: string } | null;
  /** Why there is no copy: "not_included" (this build), or what is wrong with it. */
  bundledMissing: string | null;
  installed: {
    version: string;
    path: string;
    /** On the console's title list; null when it could not be read. */
    registered: boolean | null;
    /** "shadowmount" or "ps5upload" when PS5Upload installed this version; null otherwise. */
    registeredBy: string | null;
  } | null;
  running: boolean;
  /** The other installer owns this install (its record matches), and PS5Upload did not make it. */
  otherInstaller: boolean;
  /** The other installer is in the middle of a change on this PS5. */
  storeBusy: boolean;
  action: PlayAppAction;
}

const str = (v: unknown): string | null => (typeof v === "string" && v ? v : null);
const ACTIONS: ReadonlySet<string> = new Set(["install", "update", "none", "newer_installed", "unavailable"]);

/** GET /api/play/app/installed's answer; null for anything else (an engine from before it). */
export function readPlayAppStatus(json: unknown): PlayAppStatus | null {
  if (!json || typeof json !== "object") return null;
  const o = json as Record<string, unknown>;
  if (o.title_id !== "FAKE99261" || typeof o.action !== "string") return null;
  const b = o.bundled as Record<string, unknown> | null | undefined;
  const i = o.installed as Record<string, unknown> | null | undefined;
  const store = (o.store ?? {}) as Record<string, unknown>;
  const installed =
    i && typeof i === "object" && str(i.version)
      ? {
          version: str(i.version)!,
          path: str(i.path) ?? "",
          registered: typeof i.registered === "boolean" ? i.registered : null,
          registeredBy: str(i.registered_by),
        }
      : null;
  return {
    bundled: b && typeof b === "object" && str(b.version) ? { version: str(b.version)! } : null,
    bundledMissing: str(o.bundled_missing),
    installed,
    running: o.running === true,
    otherInstaller: store.managed === true && !installed?.registeredBy,
    storeBusy: store.busy === true,
    action: (ACTIONS.has(o.action) ? o.action : "unavailable") as PlayAppAction,
  };
}

export type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** One line about the app on this PS5. */
export function playAppStatusText(tr: Tr, s: PlayAppStatus): string {
  const have = s.bundled?.version;
  if (!s.installed) {
    if (!have) return tr("pai_none_here", undefined, "Not on this PS5. This build of PS5Upload doesn't include PS5Upload Play.");
    return tr("pai_not_installed", { version: have }, `Not on this PS5 yet. PS5Upload can put version ${have} on it.`);
  }
  const on = s.installed.version;
  switch (s.action) {
    case "update":
      return tr("pai_update", { on, version: have ?? "" }, `Version ${on} is on this PS5. Version ${have ?? ""} is ready to install.`);
    case "newer_installed":
      return tr("pai_newer_on_ps5", { on }, `Version ${on} is on this PS5, newer than the one in this PS5Upload.`);
    default:
      return tr("pai_installed", { on }, `Version ${on} is on this PS5.`);
  }
}

/** What a refusal or failure means, in words. */
export function playAppErrorText(tr: Tr, reason: string | null, error: string | null): string {
  switch (reason) {
    case "store_busy":
      return tr("pai_err_store_busy", undefined, "Another app is changing apps on this PS5. Open it to let it finish, then try again.");
    case "busy":
      return tr("pai_err_busy", undefined, "PS5Upload Play is already being installed or removed on this PS5.");
    case "app_running":
      return tr("pai_err_running", undefined, "PS5Upload Play is open on the PS5 and wouldn't close. Close it on the PS5, then try again.");
    case "not_newer":
      return tr("pai_err_not_newer", undefined, "The PS5 has a newer version than the one in this PS5Upload.");
    case "unexpected":
      return tr("pai_err_unexpected", undefined, "Something else is in PS5Upload Play's folder on the PS5, so nothing was changed.");
    case "backup_left":
      return tr("pai_err_backup", undefined, "An earlier update left a copy behind on the PS5, so nothing was changed.");
    case "work_scanned":
      return tr("pai_err_scanned", undefined, "The PS5's game folders are set up so that the install's work folder would show up as a game. Nothing was changed.");
    case "not_included":
      return tr("pai_err_not_included", undefined, "This build of PS5Upload doesn't include PS5Upload Play.");
    case "launch":
      return tr("pai_err_launch", { error: error ?? "" }, `The PS5 didn't start PS5Upload Play: ${error ?? ""}`);
    default:
      return error ? tr("pai_err_other", { error }, `Didn't work: ${error}`) : tr("pai_err_engine", undefined, "Didn't work. Check that the PS5 is connected.");
  }
}

export interface PlayAppResult {
  ok: boolean;
  reason: string | null;
  error: string | null;
  json: Record<string, unknown>;
}

async function call(path: string, body?: object): Promise<PlayAppResult> {
  try {
    const r = await fetch(`${getEngineUrl()}${path}`, {
      method: body ? "POST" : "GET",
      headers: body ? { "Content-Type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    const json = ((await r.json().catch(() => null)) ?? {}) as Record<string, unknown>;
    // An install whose files are in place but that the PS5 would not list says so in `register_error`.
    const ok = r.ok && json.ok !== false;
    return { ok, reason: str(json.reason), error: str(json.error) ?? str(json.register_error), json };
  } catch (e) {
    return { ok: false, reason: null, error: e instanceof Error ? e.message : null, json: {} };
  }
}

export const playAppInstallApi = {
  /** The app on this PS5; null when the engine has no such route or the PS5 doesn't answer. */
  status: async (addr: string): Promise<PlayAppStatus | null> => {
    const r = await call(`/api/play/app/installed?addr=${encodeURIComponent(addr)}`);
    return r.ok ? readPlayAppStatus(r.json) : null;
  },
  install: (addr: string, opts: { confirmStore?: boolean } = {}) =>
    call("/api/play/app/install", { addr, confirm_store: opts.confirmStore === true }),
  remove: (addr: string, opts: { deleteData?: boolean; confirmStore?: boolean } = {}) =>
    call("/api/play/app/remove", {
      addr,
      delete_data: opts.deleteData === true,
      confirm_store: opts.confirmStore === true,
    }),
  open: (addr: string) => call("/api/play/app/open", { addr }),
};
