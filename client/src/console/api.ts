/**
 * The console page's client for the helper's web API.
 *
 * Every route is POST + JSON and needs the per-start token in X-PS5U-Token.
 * The page reads the token once from GET /api/token (same origin; another
 * site's page cannot read that response). A 401 {"error":"token"} means the
 * helper restarted and issued a new token: the page must be reloaded, so
 * onTokenExpired listeners hear about it (the ReloadBanner).
 */

import { PAGE_VERSION } from "./offline/update.ts";

export const TOKEN_HEADER = "X-PS5U-Token";

/** A failed call. status 0 = the request never got an answer. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** The reply's JSON, when the helper sent one with the error (a failed SMB job comes back whole). */
  readonly body: unknown;
  constructor(status: number, code: string, body: unknown = null) {
    super(`${status} ${code}`);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

/** The helper is not answering (network failure) or is shutting down (503 stopping). */
export function isOffline(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 0 || err.code === "stopping");
}

const expiredListeners = new Set<() => void>();

/** Called on every 401 from the API. Returns the unsubscribe function. */
export function onTokenExpired(fn: () => void): () => void {
  expiredListeners.add(fn);
  return () => {
    expiredListeners.delete(fn);
  };
}

let tokenPromise: Promise<string> | null = null;

/** A token read that has not answered by then counts as "nothing answered" (the offline screen). */
export const TOKEN_TIMEOUT_MS = 4000;

/**
 * GET /api/token's URL. `?page=` tells the helper this page reloads itself when it is older
 * (offline/selfUpdate.ts): a read without it is an older stored page, which the helper nudges into
 * reloading once (payload/include/webui_offline.h).
 */
export function tokenUrl(page: string = PAGE_VERSION): string {
  return `/api/token?page=${encodeURIComponent(page || "dev")}`;
}

let lastVersion: string | null = null;

/** The helper's version from its last token reply (helpers from 11.6.1 send it), else null. */
export function helperVersionFromToken(): string | null {
  return lastVersion;
}

async function readToken(): Promise<{ token: string; version: string | null }> {
  let res: Response;
  const stop = typeof AbortController === "function" ? new AbortController() : null;
  const timer = setTimeout(() => stop?.abort(), TOKEN_TIMEOUT_MS);
  try {
    res = await fetch(tokenUrl(), { method: "GET", credentials: "same-origin", cache: "no-store", signal: stop?.signal });
  } catch {
    throw new ApiError(0, "offline");
  } finally {
    clearTimeout(timer);
  }
  if (!res.ok) throw new ApiError(res.status, res.status === 503 ? "stopping" : "token_unavailable");
  // Anything but JSON (another server on the port, a captive page) is not the helper.
  const body = (await res.json().catch(() => {
    throw new ApiError(0, "offline");
  })) as { token?: unknown; version?: unknown } | null;
  if (!body || typeof body !== "object") throw new ApiError(0, "offline");
  if (typeof body.token !== "string" || !body.token) throw new ApiError(res.status, "token_unavailable");
  const version = typeof body.version === "string" && body.version ? body.version : null;
  if (version) lastVersion = version;
  return { token: body.token, version };
}

async function fetchToken(): Promise<string> {
  return (await readToken()).token;
}

/**
 * The helper's version, read now (a fresh token read; the token this page holds is kept). Null when
 * the helper did not answer or is too old to say.
 */
export async function readHelperVersion(): Promise<string | null> {
  try {
    return (await readToken()).version;
  } catch {
    return null;
  }
}

function token(): Promise<string> {
  if (!tokenPromise) {
    tokenPromise = fetchToken();
    // A failed read is not cached: the next call asks again.
    tokenPromise.catch(() => {
      tokenPromise = null;
    });
  }
  return tokenPromise;
}

/**
 * Is the helper there? Reads the token (kept for the calls that follow). False only when nothing
 * answered like the helper (the page came from the browser's offline copy: the service worker or
 * the appcache) or it is shutting down; any other answer means it is running.
 */
export async function probeHelper(): Promise<boolean> {
  try {
    await token();
    return true;
  } catch (e) {
    return !isOffline(e);
  }
}

/** The ApiError for a reply's status and its {"error":..} body (`http_<status>` without one). */
function errorOf(status: number, json: unknown): ApiError {
  const code =
    json && typeof (json as { error?: unknown }).error === "string" ? (json as { error: string }).error : `http_${status}`;
  return new ApiError(status, code, json ?? null);
}

function tokenExpired(): ApiError {
  for (const fn of Array.from(expiredListeners)) fn();
  return new ApiError(401, "token");
}

/**
 * A request with no whole answer by then is given up on: ApiError(0, "timeout"), which reads as
 * offline ("The console did not answer"), so a stuck helper or a dead link ends in a message and a
 * retry instead of "Loading…" for ever. The engine's default for the same calls is 30 s too.
 */
export const CALL_TIMEOUT_MS = 30_000;

/** The routes that answer only after real work on the helper (the rest answer well inside the default). */
const SLOW_ROUTES: Readonly<Record<string, number>> = {
  // JOB_COPY walks its source before it answers: minutes for a huge tree on a slow drive.
  "/api/fs/copy": 600_000,
  // Sony's launcher and installer queue behind one lock (the engine gives these 60 s), and the helper
  // may then wait for a title's folders to go.
  "/api/apps/launch": 90_000,
  "/api/apps/register": 90_000,
  "/api/apps/uninstall": 90_000,
  "/api/app/close": 90_000,
  "/api/volumes/eject": 60_000,
  "/api/volumes/unmount": 60_000,
  // Through the console's payload loader, which is retried.
  "/api/payload/load": 60_000,
  "/api/library/scan": 60_000,
};

/** A network-share call may start the SMB daemon first (loader retries, its start-up), then sign in. */
const SMB_TIMEOUT_MS = 60_000;

/** How long `route` is given. */
export function callTimeoutMs(route: string): number {
  return SLOW_ROUTES[route] ?? (route.startsWith("/api/smb/") ? SMB_TIMEOUT_MS : CALL_TIMEOUT_MS);
}

interface PostOptions {
  /** JSON body. */
  body?: object;
  /** Raw bytes instead, for a streamed route. */
  raw?: Blob;
  signal?: AbortSignal;
  keepalive?: boolean;
}

/**
 * POST to `route` and `read` the 2xx reply; any other answer is its ApiError. The deadline covers the
 * body too (a reply can stall half way), and is a race as well as an abort, so it holds where fetch
 * ignores the signal.
 */
async function post<T>(route: string, o: PostOptions, read: (res: Response) => Promise<T>): Promise<T> {
  const tok = await token();
  const stop = typeof AbortController === "function" ? new AbortController() : null;
  const onOuter = () => stop?.abort();
  if (o.signal?.aborted) stop?.abort();
  else o.signal?.addEventListener("abort", onOuter);
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      reject(new ApiError(0, "timeout"));
      stop?.abort();
    }, callTimeoutMs(route));
  });
  const work = async () => {
    let res: Response;
    try {
      res = await fetch(route, {
        method: "POST",
        credentials: "same-origin",
        cache: "no-store",
        headers: { "Content-Type": o.raw ? "application/octet-stream" : "application/json", [TOKEN_HEADER]: tok },
        body: o.raw ?? JSON.stringify(o.body ?? {}),
        signal: stop?.signal ?? o.signal,
        ...(o.keepalive ? { keepalive: true } : {}),
      });
    } catch {
      throw new ApiError(0, "offline");
    }
    if (res.status === 401) throw tokenExpired();
    if (!res.ok) throw errorOf(res.status, await res.json().catch(() => null));
    return read(res);
  };
  try {
    return await Promise.race([work(), deadline]);
  } finally {
    clearTimeout(timer);
    o.signal?.removeEventListener("abort", onOuter);
  }
}

const readJson = async <T>(res: Response): Promise<T> => (await res.json().catch(() => null)) as T;

/** POST `body` as JSON to `route` and return the parsed JSON reply. */
export async function call<T>(route: string, body?: object): Promise<T> {
  return post<T>(route, { body }, readJson);
}

/**
 * POST with `keepalive`: the request is sent even if the page is being hidden or
 * left right after (the input log's last batch). Bodies stay small (≤ 64 KiB is the
 * browser's keepalive limit; the input log sends ≤ 4 KiB).
 */
export async function callKeepalive(route: string, body?: object): Promise<void> {
  await post(route, { body, keepalive: true }, async () => undefined);
}

/** POST `body` as JSON to `route` and return the raw reply (/api/fs/read's bytes). `signal` aborts it. */
export async function callBlob(route: string, body?: object, signal?: AbortSignal): Promise<Blob> {
  return post(route, { body, signal }, (res) => res.blob());
}

/** POST raw bytes to a streamed route (/api/fs/save-text) and return the parsed JSON reply. */
export async function callRaw<T>(route: string, body: Blob): Promise<T> {
  return post<T>(route, { raw: body }, readJson);
}

/**
 * POST `body` with XMLHttpRequest (fetch reports no upload progress): `onProgress(sent, total)` as the
 * bytes leave, `signal` aborts it (ApiError 0 "aborted"). The parsed JSON reply.
 */
export async function sendWithProgress<T>(
  route: string,
  body: Blob,
  onProgress?: (sent: number, total: number) => void,
  signal?: AbortSignal,
): Promise<T> {
  const tok = await token();
  return new Promise<T>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    const onAbort = () => xhr.abort();
    const done = () => signal?.removeEventListener("abort", onAbort);
    xhr.open("POST", route);
    xhr.setRequestHeader(TOKEN_HEADER, tok);
    xhr.setRequestHeader("Content-Type", "application/octet-stream");
    xhr.upload.onprogress = (e) => onProgress?.(e.loaded, e.lengthComputable ? e.total : body.size);
    xhr.onload = () => {
      done();
      let json: unknown;
      try {
        json = JSON.parse(xhr.responseText);
      } catch {
        json = null;
      }
      if (xhr.status === 401) reject(tokenExpired());
      else if (xhr.status < 200 || xhr.status >= 300) reject(errorOf(xhr.status, json));
      else resolve(json as T);
    };
    xhr.onerror = () => {
      done();
      reject(new ApiError(0, "offline"));
    };
    xhr.onabort = () => {
      done();
      reject(new ApiError(0, "aborted"));
    };
    if (signal?.aborted) {
      reject(new ApiError(0, "aborted"));
      return;
    }
    signal?.addEventListener("abort", onAbort);
    xhr.send(body);
  });
}
