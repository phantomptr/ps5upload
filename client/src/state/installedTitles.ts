import { useEffect, useState } from "react";

import { appsInstalled, type InstalledTitle } from "../api/ps5";
import { hostOf, transferAddr } from "../lib/addr";

/** Each console's registered titles, kept so coming back shows them at once while they are
 *  read again. */
const cache = new Map<string, InstalledTitle[]>();

/** The titles registered on `host` (undefined: no console, or its helper is not up): what was
 *  read last at once, then a fresh read. Null until the first read answers. */
export function useInstalledTitles(host: string | undefined): InstalledTitle[] | null {
  const key = host ? hostOf(host) : "";
  const [titles, setTitles] = useState<InstalledTitle[] | null>(() => cache.get(key) ?? null);
  useEffect(() => {
    setTitles(cache.get(key) ?? null);
    if (!host) return;
    let live = true;
    const read = () =>
      appsInstalled(transferAddr(host))
        .then((r) => {
          cache.set(key, r.titles);
          if (live) setTitles(r.titles);
        })
        .catch(() => {});
    void read();
    // An install or uninstall that finishes while this is on screen changes the answer.
    window.addEventListener("ps5upload:library:invalidate", read);
    return () => {
      live = false;
      window.removeEventListener("ps5upload:library:invalidate", read);
    };
  }, [host, key]);
  return titles;
}
