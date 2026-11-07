// fetch against the engine with a limit. Without one, an engine that is stuck or gone leaves a
// screen's spinner running for good, while the same engine state gives an error elsewhere.

import { trStatic } from "./trStatic";

/** What a plain request to the engine may take. The desktop proxies stop at the same point. */
export const ENGINE_TIMEOUT_MS = 60_000;
/** A route that does its work before answering: a save zipped, a backup taken, a tool fetched. */
export const ENGINE_LONG_TIMEOUT_MS = 10 * 60_000;

export interface EngineFetchInit extends RequestInit {
  timeoutMs?: number;
}

/** fetch with a limit, turning the timeout into an error that says what happened. A caller with
 *  its own `signal` decides itself when to give up. The limit covers the body too. */
export async function engineFetch(url: string, init: EngineFetchInit = {}): Promise<Response> {
  const { timeoutMs = ENGINE_TIMEOUT_MS, ...rest } = init;
  if (rest.signal) return fetch(url, rest);
  try {
    return await fetch(url, { ...rest, signal: AbortSignal.timeout(timeoutMs) });
  } catch (e) {
    throw isTimeout(e) ? noAnswer(timeoutMs, e) : e;
  }
}

function isTimeout(e: unknown): boolean {
  const name = e instanceof Error || e instanceof DOMException ? e.name : "";
  return name === "TimeoutError" || name === "AbortError";
}

function noAnswer(ms: number, cause: unknown): Error {
  const wrapped = new Error(
    trStatic(
      "engine_no_answer",
      "The engine did not answer within {seconds} seconds. Check that it is still running, then try again.",
      { seconds: Math.round(ms / 1000) },
    ),
  );
  (wrapped as Error & { cause?: unknown }).cause = cause;
  return wrapped;
}

/** The answer a route gave, read back from the engine's operation table. */
export interface EngineLongAnswer {
  status: number;
  ok: boolean;
  body: Record<string, unknown>;
}

export interface EngineLongPostOptions {
  /** Between two reads of the operation's state. */
  pollMs?: number;
  /** How long the engine may go without answering a read before this gives up. The operation
   *  itself has no limit: an archive of thousands of files extracts for an hour or more. */
  silentMs?: number;
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

function asObject(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : {};
}

/** POST a route that works until it is done (a console copy, move, delete, extract or dump) and
 *  wait for its answer however long it takes (#712). The engine runs it in the background
 *  (`Prefer: respond-async`, answered 202 with an id) and `GET /api/ops/{id}` says when it is done,
 *  so no single request is held open past {@link ENGINE_TIMEOUT_MS}. It only gives up when the
 *  engine itself stops answering for `silentMs`, or forgets the operation (it restarted). An engine
 *  that answers the POST itself is taken at its word. */
export async function engineLongPost(
  baseUrl: string,
  path: string,
  body: unknown,
  opts: EngineLongPostOptions = {},
): Promise<EngineLongAnswer> {
  const pollMs = opts.pollMs ?? 1000;
  const silentMs = opts.silentMs ?? 2 * ENGINE_TIMEOUT_MS;
  const res = await engineFetch(`${baseUrl}${path}`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Prefer: "respond-async" },
    body: JSON.stringify(body),
  });
  const first = asObject(await res.json().catch(() => ({})));
  if (res.status !== 202 || typeof first.op_id !== "string") {
    return { status: res.status, ok: res.ok, body: first };
  }
  const statusUrl = `${baseUrl}/api/ops/${encodeURIComponent(first.op_id)}`;
  let lastAnswer = Date.now();
  // A one-file delete is done in milliseconds: look again soon, then settle on `pollMs`.
  let wait = Math.min(50, pollMs);
  for (;;) {
    await sleep(wait);
    wait = Math.min(wait * 2, pollMs);
    let r: Response;
    try {
      r = await engineFetch(statusUrl, { timeoutMs: Math.min(silentMs, ENGINE_TIMEOUT_MS) });
    } catch (e) {
      if (Date.now() - lastAnswer < silentMs) continue;
      throw noAnswer(silentMs, e);
    }
    if (r.status === 404) {
      throw new Error(
        trStatic(
          "engine_op_lost",
          "The engine restarted while this was running, so how it ended is unknown. Look at the files on the console before you start it again.",
        ),
      );
    }
    if (!r.ok) {
      // A proxy in between failing for a moment: the operation goes on.
      if (Date.now() - lastAnswer < silentMs) continue;
      throw noAnswer(silentMs, new Error(`HTTP ${r.status}`));
    }
    lastAnswer = Date.now();
    const op = asObject(await r.json().catch(() => ({})));
    if (op.status !== "done") continue;
    const status = typeof op.http_status === "number" ? op.http_status : 500;
    return { status, ok: status >= 200 && status < 300, body: asObject(op.body) };
  }
}
