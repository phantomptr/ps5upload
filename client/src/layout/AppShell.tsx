import { Outlet, useLocation, useNavigate } from "react-router-dom";
import { useEffect, useRef } from "react";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";
import OperationBar from "./OperationBar";
import { useConnectionStore } from "../state/connection";
import { useUpdateStore } from "../state/update";
import { engineApi } from "../api/engine";
import { payloadCheck } from "../api/ps5";
import { installActivityWiring } from "../state/activityWiring";
import { ensureRosterMigrated, useRosterStore } from "../state/roster";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauriEnv } from "../lib/tauriEnv";
import { useDocumentVisible } from "../lib/visibility";
import { useScheduleRunner } from "../state/schedules";
import { pushNotification, runNotificationAutoPrune } from "../state/notifications";
import { powerTick } from "../api/ps5";
import { CommandPalette } from "../components/CommandPalette";
import { ShortcutsOverlay } from "../components/ShortcutsOverlay";
import { useWindowStatePersistence } from "../lib/windowState";
import { installPlayTimeAccumulator } from "../state/playTime";

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
  const visible = useDocumentVisible();

  useEffect(() => {
    if (!visible) return;
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
  }, [setStatus, visible]);

  useEffect(() => {
    if (!visible) return;
    if (!host || !host.trim()) {
      // Host cleared — wipe every payload-derived field so a new
      // host doesn't inherit stale assertions from the previous one.
      // Pre-2.2.52 we forgot `ucredElevated` here, leaving e.g. a
      // green "Kernel R/W: available" pill next to a now-cleared
      // version/kernel pair.
      setStatus({
        payloadStatus: "unknown",
        payloadStatusHost: null,
        payloadVersion: null,
        ps5Kernel: null,
        ucredElevated: null,
      });
      return;
    }
    // Capture the host this probe is running against — write it
    // alongside payloadStatus so consumers (e.g. Connection screen's
    // auto-heal effect) can check that the result hasn't been
    // superseded by a host change between probe-fire and store-write.
    const probedHost = host;
    let cancelled = false;
    const tick = async () => {
      try {
        const s = await payloadCheck(probedHost);
        if (!cancelled) {
          setStatus({
            payloadStatus: s.reachable ? "up" : "down",
            payloadStatusHost: probedHost,
            // Keep the last-known version while payload is briefly
            // unreachable (e.g. a single failed poll) so the UI doesn't
            // flicker; only clear when we never had a value.
            payloadVersion: s.payloadVersion,
            ps5Kernel: s.ps5Kernel,
            ucredElevated: s.ucredElevated,
            // Clear the "rechecking…" indicator any time a tick lands
            // a real result. Connection's handleSend sets probing=true
            // on Replace payload click; this is the safety-net path
            // that clears it if handleSend's own probe never got a
            // chance to (e.g. user navigated away from Connection
            // mid-flight, leaving the flag latched).
            payloadProbing: false,
          });
          // Update the roster row for the active profile so the
          // sidebar reflects the freshest firmware/payload info
          // without making the user open a settings dialog.
          if (s.reachable) {
            const active = useRosterStore.getState();
            if (active.active_id) {
              active.noteSeen(active.active_id, {
                kernel: s.ps5Kernel,
                payload: s.payloadVersion,
              });
            }
          }
        }
      } catch {
        if (!cancelled) {
          setStatus({
            payloadStatus: "down",
            payloadStatusHost: probedHost,
            // Probe failed — also clear the probing flag so we don't
            // dangle a "rechecking…" badge forever on a host that's
            // gone offline mid-replace.
            payloadProbing: false,
          });
        }
      }
    };
    tick();
    const h = setInterval(tick, 10000);
    return () => {
      cancelled = true;
      clearInterval(h);
    };
  }, [host, setStatus, visible]);
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

/** App-wide drag-drop listener that auto-routes .pkg files to the
 *  Install Package screen. Other file types fall through to the
 *  per-screen handlers (Upload screen subscribes to the same event
 *  separately). The auto-route only fires when the user is NOT
 *  already on /install-package — avoids stomping on the screen's
 *  own picker. */
function usePkgAutoRoute() {
  const navigate = useNavigate();
  const location = useLocation();
  useEffect(() => {
    if (!isTauriEnv()) return; // browser-only dev/test contexts skip Tauri APIs
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (cancelled) return;
      if (e.payload.type !== "drop") return;
      const first = e.payload.paths?.[0];
      if (!first) return;
      if (!first.toLowerCase().endsWith(".pkg")) return;
      if (location.pathname === "/install-package") return;
      // Pass the picked path via navigation state — InstallPackage
      // can pick it up and pre-fill its source field.
      navigate("/install-package", { state: { droppedPath: first } });
    });
    p.then((fn) => {
      // Tauri unlisten can throw if the webview already tore down its
      // listener table (HMR, parent destroyed, …). Cleanup is best-
      // effort so we swallow these specific failures.
      if (cancelled) { try { fn(); } catch { /* ignore */ } }
      else unlisten = fn;
    }).catch(() => { /* subscribe-time rejection: nothing to clean */ });
    return () => {
      cancelled = true;
      if (unlisten) {
        try { unlisten(); } catch { /* ignore */ }
      }
    };
  }, [navigate, location.pathname]);
}

/** Persist the last-active route across launches. On mount, if the
 *  user is at "/" or "/whats-new" but a stored route exists, restore
 *  it. The stored route is otherwise updated on every navigation
 *  with a debounce so back/forward chains don't write per click.
 *
 *  Skipped routes:
 *   - /first-run — wizard; users should re-enter through Settings
 *   - /whats-new — landing default; redundant
 */
const LAST_ROUTE_KEY = "ps5upload.last_route";
const SKIP_RESTORE_ROUTES = new Set(["/first-run", "/whats-new", "/"]);

function useRoutePersistence() {
  const navigate = useNavigate();
  const location = useLocation();
  const restoredRef = useRef(false);

  // One-time restore on first paint.
  useEffect(() => {
    if (restoredRef.current) return;
    restoredRef.current = true;
    if (typeof window === "undefined") return;
    if (!SKIP_RESTORE_ROUTES.has(location.pathname)) return;
    let stored: string | null;
    try {
      stored = window.localStorage.getItem(LAST_ROUTE_KEY);
    } catch {
      return;
    }
    if (stored && !SKIP_RESTORE_ROUTES.has(stored)) {
      navigate(stored, { replace: true });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Debounced write of the current pathname.
  useEffect(() => {
    if (typeof window === "undefined") return;
    if (SKIP_RESTORE_ROUTES.has(location.pathname)) return;
    const id = window.setTimeout(() => {
      try {
        window.localStorage.setItem(LAST_ROUTE_KEY, location.pathname);
      } catch {
        // best-effort
      }
    }, 500);
    return () => window.clearTimeout(id);
  }, [location.pathname]);
}

export default function AppShell() {
  useStatusPolling();
  useUpdateCheckOnMount();
  useRoutePersistence();
  useWindowStatePersistence();
  usePkgAutoRoute();
  // Schedule runner — fires while window open. Browser-side; for
  // true cron behaviour the user needs an external scheduler.
  useScheduleRunner((sch) => {
    if (sch.action === "notif") {
      pushNotification("info", `Scheduled: ${sch.label}`, {
        body: sch.body ?? "Schedule fired.",
      });
    } else if (sch.action === "power_tick") {
      const host = useConnectionStore.getState().host;
      if (host?.trim()) {
        void powerTick(`${host.trim()}:9114`).catch(() => {
          // best-effort
        });
        pushNotification("info", `Scheduled: ${sch.label}`, {
          body: `Sent powerTick to ${host.trim()}.`,
        });
      }
    }
  });
  // Subscribe-once: wires the per-feature stores (transfer, FS bulk
  // op, FS download) into the cross-screen activity history. Safe to
  // call on every render because installActivityWiring is idempotent.
  useEffect(() => {
    installActivityWiring();
    // Migrate single-host users into the multi-PS5 roster on first
    // start. Idempotent — no-op when the roster is already populated.
    ensureRosterMigrated();
    // Subscribe-once: cross-store accumulator that credits running
    // titles with elapsed wall-clock between updates. Idempotent.
    installPlayTimeAccumulator();
    // Notification auto-prune: run once at mount + every 6 hours.
    // Keeps the inbox from accumulating year-old "upload finished"
    // entries that nobody will ever revisit.
    runNotificationAutoPrune();
    const pruneTimer = window.setInterval(
      runNotificationAutoPrune,
      6 * 3600 * 1000,
    );
    return () => window.clearInterval(pruneTimer);
  }, []);
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
      <OperationBar />
      <StatusBar />
      <CommandPalette />
      <ShortcutsOverlay />
    </div>
  );
}
