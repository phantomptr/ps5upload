import { PayloadSendRefused, type HelperSendOptions } from "../api/ps5";
import { invoke } from "./invokeLogged";

/** The browser build's send: `payload_bundled_path` and `payload_send` are desktop-only commands,
 *  so the engine, which owns the bytes and the socket there, sends its own bundled helper. Throws when the engine
 *  could not (no helper image, loader not answering), so a caller that records "a helper was
 *  sent" (lib/helperSendGate) records nothing for a send that never happened. A refusal of the
 *  engine's helper gate (a send moments ago, a busy console, the hourly limit...) throws
 *  `PayloadSendRefused` with its code. */
export async function engineSendsHelper(ip: string, opts: HelperSendOptions = {}): Promise<void> {
  const r = (await invoke("payload_restore", {
    ip,
    mode: opts.mode,
    force: opts.force,
    replace: opts.replace,
    appVersion: opts.appVersion,
  })) as {
    ok?: boolean;
    error?: string;
    refused?: string;
    forceable?: boolean;
    retry_after_ms?: number | null;
  };
  if (r?.ok) return;
  if (r?.refused) {
    throw new PayloadSendRefused(r.refused, {
      message: r.error,
      forceable: r.forceable === true,
      retryAfterMs: typeof r.retry_after_ms === "number" ? r.retry_after_ms : null,
    });
  }
  throw new Error(r?.error ?? "the engine has no helper to send");
}
