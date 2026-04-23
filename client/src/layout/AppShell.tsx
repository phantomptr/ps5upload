import { Outlet } from "react-router-dom";
import { useEffect } from "react";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";
import { useConnectionStore } from "../state/connection";
import { useUpdateStore } from "../state/update";
import { engineApi } from "../api/engine";
import { payloadCheck } from "../api/ps5";

/** Background status polling for the engine + payload dots in the
 *  status bar. Runs for the lifetime of the app so the indicators
 *  reflect current state regardless of which screen is visible.
 *
 *  - Engine: localhost `/api/jobs`, every 5s. Fast; doesn't touch PS5.
 *  - Payload: the PS5's :9113 via `payload_check`, every 10s, and only
 *    when a host is configured (no point spamming DOWN probes against
 *    the default IP if the user hasn't entered theirs). */
function useStatusPolling() {
  const host = useConnectionStore((s) => s.host);
  const setStatus = useConnectionStore((s) => s.setStatus);

  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      const up = await engineApi.ping();
      if (!cancelled) setStatus({ engineStatus: up ? "up" : "down" });
    };
    tick();
    const h = setInterval(tick, 5000);
    return () => {
      cancelled = true;
      clearInterval(h);
    };
  }, [setStatus]);

  useEffect(() => {
    if (!host || !host.trim()) {
      setStatus({
        payloadStatus: "unknown",
        payloadVersion: null,
        ps5Kernel: null,
      });
      return;
    }
    let cancelled = false;
    const tick = async () => {
      try {
        const s = await payloadCheck(host);
        if (!cancelled) {
          setStatus({
            payloadStatus: s.reachable ? "up" : "down",
            // Keep the last-known version while payload is briefly
            // unreachable (e.g. a single failed poll) so the UI doesn't
            // flicker; only clear when we never had a value.
            payloadVersion: s.payloadVersion,
            ps5Kernel: s.ps5Kernel,
          });
        }
      } catch {
        if (!cancelled) setStatus({ payloadStatus: "down" });
      }
    };
    tick();
    const h = setInterval(tick, 10000);
    return () => {
      cancelled = true;
      clearInterval(h);
    };
  }, [host, setStatus]);
}

/** Fire a TTL-gated update check on mount. The store debounces to
 *  one check per day and caches in sessionStorage, so this is safe to
 *  call unconditionally — subsequent tab switches hit cache. */
function useUpdateCheckOnMount() {
  const ensureChecked = useUpdateStore((s) => s.ensureChecked);
  useEffect(() => {
    // Defer past first paint so the app window renders before we
    // touch the network. The updater's endpoint is GitHub, so a slow
    // DNS would otherwise delay the initial UI by up to a few seconds.
    const id = window.setTimeout(() => {
      void ensureChecked();
    }, 1500);
    return () => window.clearTimeout(id);
  }, [ensureChecked]);
}

export default function AppShell() {
  useStatusPolling();
  useUpdateCheckOnMount();
  return (
    <div className="flex h-full flex-col bg-[var(--color-surface)] text-[var(--color-text)]">
      <div className="flex min-h-0 flex-1">
        <Sidebar />
        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <div className="flex-1 overflow-auto">
            <Outlet />
          </div>
        </main>
      </div>
      <StatusBar />
    </div>
  );
}
