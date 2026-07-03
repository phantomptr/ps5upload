import {
  invoke as rawInvoke,
  type InvokeArgs,
  type InvokeOptions,
} from "@tauri-apps/api/core";

import { log } from "../state/logs";
import { isTauriEnv } from "./tauriEnv";
import { browserInvoke } from "./browserInvoke";

/**
 * Drop-in replacement for Tauri's `invoke` that leaves a log breadcrumb for
 * every command. This is the single highest-leverage logging change in the
 * app: swapping the import in `api/ps5.ts` instruments *every* PS5/Tauri
 * command at once, so a bug report captures:
 *
 *   - at `trace`: one line per command (the full call breadcrumb a
 *     "set level to trace, then reproduce" session needs — previously there
 *     were zero trace calls, so trace mode surfaced nothing), and
 *   - at `warn` (always recorded at the default level): every command that
 *     REJECTS, with the command name + error. Command failures used to be
 *     visible only in the caller's UI state and vanished on a crash.
 *
 * IMPORTANT: never route the persistent-log sink's own `diag_log_append`
 * command through here — that would recurse (logging triggers a flush which
 * invokes the command which logs …). `state/logs.ts` deliberately keeps the
 * raw `invoke`.
 *
 * In a browser (non-Tauri) environment, Tauri IPC is unavailable. This
 * function delegates to `browserInvoke` instead, which translates each
 * command into an HTTP `fetch()` against the engine origin.
 */
export async function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  options?: InvokeOptions,
): Promise<T> {
  try {
    let result: T;
    if (!isTauriEnv()) {
      // Browser path — translate IPC → HTTP fetch.
      // `options` (Tauri Channel / transferable) has no browser equivalent;
      // commands that use it are native-only and already gated by isTauriEnv()
      // at their call sites, so they will reach BrowserUnsupportedError before
      // options would matter.
      result = await browserInvoke<T>(
        cmd,
        (args ?? {}) as Record<string, any>,
      );
    } else {
      // Tauri path — forward only the args we were given; passing a trailing
      // `undefined` options arg would change the observable call shape.
      result =
        options === undefined
          ? await rawInvoke<T>(cmd, args)
          : await rawInvoke<T>(cmd, args, options);
    }
    // Cheap, name-only at trace — the value/args could be large (manifests,
    // file lists), and the point of the breadcrumb is the sequence of calls.
    log.trace("cmd", cmd);
    return result;
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    log.warn("cmd", `${cmd} failed: ${msg}`);
    throw e;
  }
}
