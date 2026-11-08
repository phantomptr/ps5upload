import { useCallback, useEffect, useState } from "react";
import { appTailnetOp, appTailnetStatus, type AppTailnetStatus } from "../api/tailnet";
import { consoleAddr } from "../lib/addr";

/** How often a screen that shows Tailscale re-reads it (the local engine; a cheap read). */
export const TAILNET_READ_MS = 10_000;

/** One console as the engine knows it on Tailscale. */
export interface ConsoleOnTailnet {
  /** Its Tailscale address (100.x.y.z). */
  ip: string;
  /** How the engine reaches it now: the home network first, Tailscale only when home does not
   *  answer ("system": through the system's Tailscale app). */
  via: "lan" | "direct" | "relayed" | "system";
  latencyMs: number;
}

/** The console (by its home address) on Tailscale, from the engine's status; null when it is not. */
export function consoleOnTailnet(st: AppTailnetStatus | null, host: string): ConsoleOnTailnet | null {
  if (!st || !host) return null;
  const want = consoleAddr(host);
  const c = (st.consoles ?? []).find((k) => consoleAddr(k.host) === want);
  if (!c || !c.ip) return null;
  const via = c.via === "direct" || c.via === "relayed" || c.via === "system" ? c.via : "lan";
  return { ip: c.ip, via, latencyMs: typeof c.latency_ms === "number" ? c.latency_ms : -1 };
}

/** This device's own Tailscale (Settings › Tailscale): on and connected, paused, or not set up. */
export function selfTailnet(st: AppTailnetStatus | null): "on" | "paused" | "connecting" | "system" | "none" {
  if (!st) return "none";
  // The system's Tailscale app carries this device (PS5Upload's own is not set up).
  if (!st.enabled && !st.signed_in && st.backend === "system") return "system";
  if (st.enabled) return st.state === "running" ? "on" : "connecting";
  // Off with a sign-in kept: paused (on again needs no sign-in).
  return st.signed_in ? "paused" : "none";
}

/**
 * The engine's Tailscale status for a screen that shows it (the console switcher, Home), re-read
 * every {@link TAILNET_READ_MS} while `active`. An engine without Tailscale reads as null.
 * `setPaused` pauses or resumes this device's own Tailscale, keeping its sign-in.
 */
export function useTailnet(active = true): {
  status: AppTailnetStatus | null;
  setPaused: (paused: boolean) => Promise<void>;
  busy: boolean;
} {
  const [status, setStatus] = useState<AppTailnetStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const read = useCallback(async () => {
    try {
      setStatus(await appTailnetStatus());
    } catch {
      setStatus(null);
    }
  }, []);
  useEffect(() => {
    if (!active) return;
    void read();
    // Foreground only: a phone in the background asks nothing.
    const id = window.setInterval(() => {
      if (typeof document === "undefined" || !document.hidden) void read();
    }, TAILNET_READ_MS);
    return () => window.clearInterval(id);
  }, [active, read]);
  const setPaused = useCallback(async (paused: boolean) => {
    setBusy(true);
    try {
      setStatus(await appTailnetOp(paused ? "disable" : "enable"));
    } finally {
      setBusy(false);
    }
  }, []);
  return { status, setPaused, busy };
}
