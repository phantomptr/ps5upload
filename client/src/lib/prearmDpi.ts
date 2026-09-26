import { invoke } from "./invokeLogged";
import { payloadCheck } from "../api/ps5";
import { log } from "../state/logs";
import { hostOf } from "./addr";
import { restoreMainPayload } from "./restoreMainPayload";

/**
 * Bring the DPI install daemon up while we KNOW the console's loader works.
 *
 * Why this exists. Applying an update needs the DPI daemon on :9040 once the
 * in-process installer is rejected, and the only way to start it is to hand
 * its ELF to the console's payload loader on :9021 — a third-party jailbreak
 * component, not ours. A 2026-09-08 bug report showed that loader accepting
 * our payload ELF and then, 255 seconds later in the same session, refusing a
 * connection. The update could not be installed because the installer could
 * never be delivered.
 *
 * Two facts make the fix cheap. DPI is a resident daemon — an accept loop that
 * never exits — so once it is up it stays up for the rest of the console's
 * uptime. And bringing it up does NOT displace the running ps5upload payload:
 * measured on both a FW 5.10 and a FW 9.60 console, :9113/:9114 stayed up and
 * answering while :9040 came online beside them in well under a second.
 *
 * So: arm it at the moment we have just proven the loader is alive, instead of
 * discovering it is dead an hour later with a 6 GB upload already committed.
 *
 * Opportunistic by design. Every failure path returns a result and logs; none
 * throws. A console whose loader is already dead when we try simply gets the
 * old behaviour, and the install-time message now says so accurately.
 */

/** What one pre-arm attempt did. */
export type PrearmOutcome =
  /** :9040 was already answering — a probe, nothing sent. */
  | "already-up"
  /** We sent the daemon and it came up, payload intact. */
  | "armed"
  /** The loader wouldn't take it. Install-time will explain why. */
  | "unavailable"
  /** The daemon came up but took the payload with it; payload re-sent. */
  | "reverted"
  /** Already attempted for this host this session. */
  | "skipped";

export interface PrearmResult {
  outcome: PrearmOutcome;
  /** The engine's reason code when `unavailable`. */
  reason?: string;
  error?: string;
}

/**
 * One attempt per host per session.
 *
 * DPI is resident, so a success needs no repeating; and a console whose loader
 * is gone would otherwise be re-probed on every reconnect for no benefit. The
 * memo holds the in-flight promise too, so two queues starting at once share a
 * single attempt rather than racing two ELF pushes at the same loader.
 */
const attempts = new Map<string, Promise<PrearmResult>>();

/** Hosts where DPI is believed to be up right now (last attempt was
 *  `armed` or `already-up`). Drives the poller's "re-arm when it drops"
 *  decision: only a console we actually got DPI onto is worth re-probing, so
 *  a loader that declined isn't polled forever. Survives `invalidatePrearm`
 *  (which only clears the once-per-session memo) — the belief is corrected by
 *  the next attempt's outcome, not by the invalidation. */
const armedHosts = new Set<string>();

/** Test seam — the memo is module state that would leak between cases. */
export function resetPrearmMemoForTests(): void {
  attempts.clear();
  armedHosts.clear();
}

/** True when DPI was last seen up on this host — i.e. it is worth watching
 *  for a later drop. */
export function dpiWasArmed(host: string): boolean {
  return armedHosts.has(hostOf(host));
}

/** Clear the once-per-session pre-arm memo for a host so the next
 *  `prearmDpiDaemon` call actually re-attempts.
 *
 *  DPI is resident, so a single arm normally lasts the console's uptime and
 *  the memo is right to fire once. But a resident daemon still dies when its
 *  process dies — a rest/wake cycle tears down userspace (the payload goes
 *  with it, and so does DPI), and the loader that hosts it can flake
 *  mid-session. After any of those DPI is gone but the memo still says
 *  "handled", so "arm early" silently stops holding. Call this on a death
 *  signal (the wake-recovery edge, or an observed :9040 drop) to re-open the
 *  arming path; the next attempt re-probes and re-sends only if needed
 *  (dpi_ensure is idempotent). */
export function invalidatePrearm(host: string): void {
  attempts.delete(hostOf(host));
}

export function prearmDpiDaemon(host: string): Promise<PrearmResult> {
  const key = hostOf(host);
  const existing = attempts.get(key);
  if (existing) return existing.then((r) => ({ ...r, outcome: "skipped" }));
  const run = attemptPrearm(key);
  attempts.set(key, run);
  return run;
}

async function attemptPrearm(ip: string): Promise<PrearmResult> {
  let ens: {
    ok?: boolean;
    listening?: boolean;
    sent?: boolean;
    reason?: string;
    error?: string;
  };
  try {
    ens = (await invoke("dpi_ensure", { ip })) as typeof ens;
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    log.info("payload", `DPI pre-arm on ${ip} could not run: ${error}`);
    return { outcome: "unavailable", error };
  }

  if (!ens.ok) {
    // Not an error the user should see now — the update-install path reports
    // it with the right guidance if and when it actually matters. Forget any
    // prior belief that DPI is up: a loader that just declined isn't a
    // console the poller should keep re-probing.
    armedHosts.delete(ip);
    log.info(
      "payload",
      `DPI pre-arm on ${ip} declined: reason=${ens.reason ?? "unknown"}` +
        (ens.error ? ` error="${ens.error}"` : ""),
    );
    return { outcome: "unavailable", reason: ens.reason, error: ens.error };
  }

  if (!ens.sent) {
    armedHosts.add(ip);
    log.info("payload", `DPI daemon already listening on ${ip}:9040`);
    return { outcome: "already-up" };
  }

  // Measured non-destructive on FW 5.10 and FW 9.60, but the set of loaders in
  // the wild is not two. A single-payload loader would have just replaced the
  // helper with the daemon, and silently leaving the console with no helper is
  // far worse than not arming at all — so confirm, and put it back if not.
  let payloadAlive: boolean;
  try {
    payloadAlive = (await payloadCheck(ip)).reachable;
  } catch {
    payloadAlive = false;
  }
  if (payloadAlive) {
    armedHosts.add(ip);
    log.info("payload", `DPI daemon armed on ${ip}:9040 (helper intact)`);
    return { outcome: "armed" };
  }

  log.warn(
    "payload",
    `DPI pre-arm on ${ip} displaced the helper — this loader runs one payload ` +
      `at a time. Restoring the helper; not pre-arming this console again.`,
  );
  // Must go through the shared restore: `payload_send` is a desktop-only
  // command, and the self-hosted web UI — the deployment this bug was
  // reported from — has neither the ELF bytes nor a socket. Sending it
  // directly here would throw and leave the console with the daemon and no
  // helper, which is strictly worse than never arming.
  await restoreMainPayload(ip);
  return { outcome: "reverted" };
}
