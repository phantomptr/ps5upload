/** How long a request that only STARTS an engine job may take. Starting is quick (the work
 *  runs as a job afterwards); a request with no answer by then means the engine is stuck or
 *  gone, and without a limit the button would spin forever. */
export const START_TIMEOUT_MS = 60_000;

/** fetch with START_TIMEOUT_MS, turning the timeout into an error that says what happened. */
export async function fetchStart(
  what: string,
  input: string,
  init: RequestInit,
  fetchImpl: (input: string, init?: RequestInit) => Promise<Response> = (i, o) => fetch(i, o),
  ms: number = START_TIMEOUT_MS,
): Promise<Response> {
  try {
    return await fetchImpl(input, { ...init, signal: AbortSignal.timeout(ms) });
  } catch (e) {
    const name = e instanceof Error || e instanceof DOMException ? e.name : "";
    if (name === "TimeoutError" || name === "AbortError") {
      const wrapped = new Error(
        `The engine did not answer within ${Math.round(ms / 1000)} seconds while starting ${what}. Check that it is still running, then try again.`,
      );
      // The target is ES2020 (no Error options); keep the original for whoever inspects it.
      (wrapped as Error & { cause?: unknown }).cause = e;
      throw wrapped;
    }
    throw e;
  }
}

/** Whether a request's failure text says it ran out of time: the desktop proxy's "operation timed
 *  out", a browser's "signal timed out", an engine call's own deadline. */
export function isRequestTimeout(message: string): boolean {
  return /timed out|TimeoutError/i.test(message);
}
