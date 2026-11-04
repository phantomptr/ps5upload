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
