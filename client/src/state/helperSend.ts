import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { isEngineUnreachable } from "../lib/engineUnreachable";

/**
 * Sending the helper to a console, kept outside the Connection screen.
 *
 * The send (check the loader, send the ELF, wait up to 20 s for the helper to answer) used to
 * live in the screen: its "Sending…" state was component state and leaving the screen
 * cancelled the wait. Switching view or console tab mid-send therefore came back to a screen
 * that had forgotten the send, with no result. Here each console's send runs to its end
 * whatever is on screen, and the screen shows whichever console it is looking at.
 */

export type HelperSendPhase = "locating" | "sending" | "waiting";

export interface HelperSendState {
  /** A send that ended well leaves nothing here: the console's stored status says "up". */
  state: "busy" | "fail";
  phase: HelperSendPhase | null;
  /** What the step shows, already in the user's language. */
  msg: string;
  startedAtMs: number;
}

interface HelperSendStore {
  byHost: Record<string, HelperSendState>;
  put: (host: string, s: HelperSendState | null) => void;
}

export const useHelperSendStore = create<HelperSendStore>((set) => ({
  byHost: {},
  put: (host, next) =>
    set((s) => {
      const key = hostOf(host);
      if (next === null) {
        if (!(key in s.byHost)) return s;
        const rest = { ...s.byHost };
        delete rest[key];
        return { byHost: rest };
      }
      return { byHost: { ...s.byHost, [key]: next } };
    }),
}));

export function helperSendFor(
  s: { byHost: Record<string, HelperSendState> },
  host: string,
): HelperSendState | null {
  return s.byHost[hostOf(host)] ?? null;
}

/** What the helper's status probe says. */
export interface HelperProbe {
  reachable: boolean;
  error?: string | null;
}

/** What the run needs from the app. Injected so it is testable without a console. */
export interface HelperSendDeps<P extends HelperProbe = HelperProbe> {
  /** "stuck" when the console's loader would take the send and never answer. */
  waitForLoader: (host: string) => Promise<string>;
  bundledPath: () => Promise<string>;
  send: (host: string, elf: string) => Promise<unknown>;
  check: (host: string) => Promise<P>;
  isNotPaired: (error: string) => boolean;
  sleep: (ms: number) => Promise<void>;
  /** The "rechecking…" badge on the console's version: on at the start, off at any end. */
  setProbing: (host: string, on: boolean) => void;
  /** The helper answered: record it up, with what it reported. */
  onUp: (host: string, probe: P) => void;
  /** The send ended well, with the message for the step. */
  onOk: (host: string, msg: string) => void;
  /** The helper answered but this app may not use it: open pairing. */
  onNotPaired: (host: string, error: string) => void;
  /** How many times the helper is asked before giving up (one a second). Default 20. */
  maxAttempts?: number;
}

/** The step's messages, bound to the user's language when Send is pressed. */
export interface HelperSendText {
  checkingLoader: string;
  stuck: string;
  sending: (elf: string) => string;
  waiting: string;
  running: string;
  /** `tail` is "" or " Last probe: <error>." */
  timeout: (tail: string) => string;
  notPaired: (error: string) => string;
  /** The app could not reach its own engine, so whether the helper started is unknown. */
  engineUnreachable: (error: string) => string;
}

export type HelperSendResult = "ok" | "fail" | "busy";

/** Sends the helper to `host` and waits for it to answer. "busy": a send to this console is
 *  already running, and nothing was started. */
export async function runHelperSend<P extends HelperProbe>(
  host: string,
  deps: HelperSendDeps<P>,
  text: HelperSendText,
): Promise<HelperSendResult> {
  const store = useHelperSendStore.getState();
  if (helperSendFor(store, host)?.state === "busy") return "busy";
  const startedAtMs = Date.now();
  const busy = (phase: HelperSendPhase, msg: string) =>
    store.put(host, { state: "busy", phase, msg, startedAtMs });
  const fail = (msg: string): HelperSendResult => {
    deps.setProbing(host, false);
    store.put(host, { state: "fail", phase: null, msg, startedAtMs });
    return "fail";
  };

  deps.setProbing(host, true);
  busy("locating", text.checkingLoader);
  if ((await deps.waitForLoader(host)) === "stuck") return fail(text.stuck);
  try {
    const elf = await deps.bundledPath();
    busy("sending", text.sending(elf));
    await deps.send(host, elf);
  } catch (e) {
    // The send itself failed (loader unreachable, ELF missing): no new helper to wait for.
    const msg = e instanceof Error ? e.message : String(e);
    return fail(isEngineUnreachable(msg) ? text.engineUnreachable(msg) : msg);
  }
  busy("waiting", text.waiting);
  // The last raw probe error, so a timeout can say why the helper looks dead.
  let lastError = "";
  // Probes that never reached this app's own engine say nothing about the console. Three in
  // a row and the wait ends with that, not with "the helper did not come up".
  let engineMisses = 0;
  await deps.sleep(1500);
  const attempts = deps.maxAttempts ?? 20;
  for (let i = 0; i < attempts; i++) {
    if (i > 0) await deps.sleep(1000);
    try {
      const probe = await deps.check(host);
      if (probe.reachable) {
        deps.onUp(host, probe);
        store.put(host, null);
        deps.onOk(host, text.running);
        return "ok";
      }
      if (probe.error) {
        lastError = probe.error;
        // The helper answered but this app may not use it yet. Sending again cannot fix
        // that: stop asking and open pairing, which explains it and offers the fix.
        if (deps.isNotPaired(probe.error)) {
          deps.onNotPaired(host, probe.error);
          return fail(text.notPaired(probe.error));
        }
      }
    } catch (e) {
      lastError = e instanceof Error ? e.message : String(e);
      if (isEngineUnreachable(lastError)) {
        engineMisses += 1;
        if (engineMisses >= 3) return fail(text.engineUnreachable(lastError));
        continue;
      }
    }
    engineMisses = 0;
  }
  if (isEngineUnreachable(lastError))
    return fail(text.engineUnreachable(lastError));
  return fail(text.timeout(lastError ? ` Last probe: ${lastError}.` : ""));
}

/** Forgets a console's failed send (the user moved on). A running one stays. */
export function clearHelperSend(host: string): void {
  const s = useHelperSendStore.getState();
  if (helperSendFor(s, host)?.state === "busy") return;
  s.put(host, null);
}
