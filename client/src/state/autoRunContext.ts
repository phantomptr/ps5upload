// What an automatic playlist run asks the console before it sends anything: what runs there and
// when it started (lib/autoLoaderGuard decides each step from it).

import { fetchHwPower, procListGet } from "../api/ps5";
import { mgmtAddr } from "../lib/addr";
import { bootEpochMs, type AutoRunContext } from "../lib/autoLoaderGuard";
import { consoleStayedSince } from "../lib/loaderSettle";

/** What is running on a console and when it booted, or nulls when it cannot be asked. */
export async function consoleBootAndProcesses(
  host: string,
): Promise<{ boot: number | null; names: string[] | null }> {
  // Never throws: a console that can't be asked is a context of nulls.
  const [power, procs] = await Promise.all([
    Promise.resolve()
      .then(() => fetchHwPower(host))
      .catch(() => null),
    Promise.resolve()
      .then(() => procListGet(mgmtAddr(host)))
      .catch(() => null),
  ]);
  const uptime = power?.operating_time_sec ?? 0;
  return {
    boot: uptime > 0 ? bootEpochMs(Date.now(), uptime) : null,
    names: procs?.ok ? procs.procs.map((p) => p.name) : null,
  };
}

/** The context for an automatic run on `host`; `known` reuses a reading the caller just made. */
export async function autoRunContext(
  host: string,
  known?: { boot: number | null; names: string[] | null },
): Promise<AutoRunContext> {
  const { boot, names } = known ?? (await consoleBootAndProcesses(host));
  return { running: names, bootMs: boot, stayedSince: (t) => consoleStayedSince(host, t) };
}
