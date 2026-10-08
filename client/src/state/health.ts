import { create } from "zustand";

import { healthScan, type HealthCheck, type HealthReport } from "../api/ps5";
import { hostOf, mgmtAddr } from "../lib/addr";

/**
 * The health scan's result per console, shared by Home (which shows what needs attention)
 * and the Health screen (which shows everything). One scan serves both, and the result is
 * still there when either screen is opened again.
 */
export interface HealthState {
  report: HealthReport | null;
  scannedAtMs: number | null;
  scanning: boolean;
  error: string | null;
}

interface Store {
  byHost: Record<string, HealthState>;
}

export const useHealthStore = create<Store>(() => ({ byHost: {} }));

export function healthFor(s: Store, host: string): HealthState | null {
  return s.byHost[hostOf(host)] ?? null;
}

function put(host: string, patch: Partial<HealthState>) {
  const key = hostOf(host);
  useHealthStore.setState((s) => ({
    byHost: {
      ...s.byHost,
      [key]: {
        ...(s.byHost[key] ?? {
          report: null,
          scannedAtMs: null,
          scanning: false,
          error: null,
        }),
        ...patch,
      },
    },
  }));
}

/** Scans `host` now. A scan already running for it is left to finish. */
export async function scanHealth(
  host: string,
  now: number = Date.now(),
): Promise<void> {
  if (!host.trim()) return;
  if (healthFor(useHealthStore.getState(), host)?.scanning) return;
  put(host, { scanning: true, error: null });
  try {
    const report = await healthScan(mgmtAddr(host.trim()));
    put(host, { report, scannedAtMs: now, scanning: false });
  } catch (e) {
    // The last report stays: an engine hiccup is not news about the console.
    put(host, {
      scanning: false,
      error: e instanceof Error ? e.message : String(e),
    });
  }
}

/** Scans unless a report younger than `maxAgeMs` is already held. */
export async function scanHealthIfStale(
  host: string,
  maxAgeMs: number,
  now: number = Date.now(),
): Promise<void> {
  const cur = healthFor(useHealthStore.getState(), host);
  if (
    cur?.report &&
    cur.scannedAtMs !== null &&
    now - cur.scannedAtMs < maxAgeMs
  )
    return;
  await scanHealth(host, now);
}

/** The checks that need attention: problems first, then warnings. */
export function healthProblems(report: HealthReport | null): HealthCheck[] {
  if (!report) return [];
  return [
    ...report.checks.filter((c) => c.status === "fail"),
    ...report.checks.filter((c) => c.status === "warn"),
  ];
}
