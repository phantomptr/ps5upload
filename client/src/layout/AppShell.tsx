import { Outlet, useLocation, useNavigate } from "react-router-dom";
import { useEffect, useRef, useState } from "react";
import { Lock, Menu, RefreshCw, X } from "lucide-react";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";
import ActivityBar from "./ActivityBar";
import { Button } from "../components/Button";
import { useConnectionStore } from "../state/connection";
import { log } from "../state/logs";
import { useUpdateStore } from "../state/update";
import { engineApi } from "../api/engine";
import { payloadCheck } from "../api/ps5";
import { installActivityWiring } from "../state/activityWiring";
import { ensureRosterMigrated, useRosterStore } from "../state/roster";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauriEnv, safeUnlisten } from "../lib/tauriEnv";
import { useDocumentVisible } from "../lib/visibility";
import { useScheduleRunner } from "../state/schedules";
import {
  pushNotification,
  runNotificationAutoPrune,
  useNotificationsStore,
} from "../state/notifications";
import { ensureOsNotificationPermission } from "../lib/osNotify";
import { powerTick } from "../api/ps5";
import { CommandPalette } from "../components/CommandPalette";
import { ShortcutsOverlay } from "../components/ShortcutsOverlay";
import { LocalPathPicker } from "../components/LocalPathPicker";
import { useWindowStatePersistence } from "../lib/windowState";
import { mgmtAddr, hostOf } from "../lib/addr";
import { useUploadQueueStore } from "../state/uploadQueue";
import { useTransferStore } from "../state/transfer";
import { useUploadSettingsStore } from "../state/uploadSettings";
import { installPlayTimeAccumulator } from "../state/playTime";
import { useTr } from "../state/lang";
import { isAndroid } from "../lib/platform";
import { localFs } from "../api/localFs";
import { getVersion } from "@tauri-apps/api/app";

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

  // Proactive health warnings — logged once per distinct condition so the bug
  // bundle flags a likely root cause (stale helper / no kernel R/W) before the
  // user even files a report, instead of leaving them to notice a subtle pill.
  const appVersionRef = useRef<string | null>(null);
  const warnedMismatchRef = useRef<string | null>(null);
  const warnedNoUcredRef = useRef<string | null>(null);
  useEffect(() => {
    void getVersion()
      .then((v) => {
        appVersionRef.current = v;
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!visible) return;
    let cancelled = false;
    const tick = async () => {
      const up = await engineApi.ping();
      if (!cancelled) {
        setStatus({
          engineStatus: up ? "up" : "down",
          ...(up ? { engineError: null } : {}),
        });
      }
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
        maxTransferStreams: null,
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
          // On a transient miss (reachable=false) payloadCheck has no
          // STATUS body to parse, so it returns version/kernel/ucred as
          // null. Writing those nulls straight through would blank the
          // UI on every single failed poll — the comment here used to
          // promise "keep the last-known version" but the code didn't
          // actually do it. Carry the prior values over instead, but
          // ONLY when they belong to THIS host: a freshly-switched host
          // must not inherit the previous PS5's firmware string.
          const prev = useConnectionStore.getState();
          const carryOver =
            !s.reachable && prev.payloadStatusHost === probedHost;
          // Log only on an up<->down TRANSITION (not every 10s poll) — a
          // helper that drops mid-upload is the #1 thing a bug report needs
          // to show, and it was previously invisible in the logs.
          const newStatus = s.reachable ? "up" : "down";
          if (
            prev.payloadStatusHost === probedHost &&
            prev.payloadStatus !== "unknown" &&
            prev.payloadStatus !== newStatus
          ) {
            if (newStatus === "down") {
              log.warn("connection", `helper went DOWN on ${probedHost}`);
            } else {
              log.info("connection", `helper came UP on ${probedHost}`);
            }
          }
          setStatus({
            payloadStatus: s.reachable ? "up" : "down",
            payloadStatusHost: probedHost,
            payloadVersion: carryOver ? prev.payloadVersion : s.payloadVersion,
            ps5Kernel: carryOver ? prev.ps5Kernel : s.ps5Kernel,
            ucredElevated: carryOver ? prev.ucredElevated : s.ucredElevated,
            maxTransferStreams: carryOver
              ? prev.maxTransferStreams
              : s.maxTransferStreams,
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

            // Health: stale helper. A payload older than the app is a frequent
            // root cause of "feature X doesn't work" — flag it once.
            const av = appVersionRef.current;
            if (av && s.payloadVersion && s.payloadVersion !== av) {
              const key = `${probedHost}:${s.payloadVersion}`;
              if (warnedMismatchRef.current !== key) {
                warnedMismatchRef.current = key;
                log.warn(
                  "health",
                  `helper version ${s.payloadVersion} != app ${av} on ${probedHost} — reload the payload (Connection → Replace)`,
                );
              }
            }

            // Health: no kernel R/W (kstuff not loaded) — install/elevation
            // features are degraded. Warn once per host; reset when it returns.
            if (s.ucredElevated === false) {
              if (warnedNoUcredRef.current !== probedHost) {
                warnedNoUcredRef.current = probedHost;
                log.warn(
                  "health",
                  `kernel R/W unavailable on ${probedHost} (kstuff not loaded) — some install/launch features degraded`,
                );
              }
            } else if (s.ucredElevated === true) {
              warnedNoUcredRef.current = null;
            }
          }
        }
      } catch (e) {
        if (!cancelled) {
          const prev = useConnectionStore.getState();
          if (
            prev.payloadStatusHost === probedHost &&
            prev.payloadStatus === "up"
          ) {
            log.warn(
              "connection",
              `helper went DOWN on ${probedHost} (probe error: ${e instanceof Error ? e.message : String(e)})`,
            );
          }
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

/** Keep the PS5 awake while an upload is in flight (default on). The PS5's
 *  auto-standby timer (shortest setting ~20 min) would otherwise drop a long
 *  upload into rest mode mid-transfer — the `spool_apply_failed` failure.
 *  While the upload queue OR the single-shot transfer is active, send a
 *  power-tick (sceSystemServicePowerTick, resets the idle timer) to every
 *  console with a running job, every few minutes. Each tick is one tiny mgmt
 *  frame; failures (payload momentarily down during auto-resume) are ignored.
 */
const KEEP_PS5_AWAKE_TICK_MS = 2 * 60 * 1000;
function useKeepPs5AwakeDuringUploads() {
  const enabled = useUploadSettingsStore((s) => s.keepPs5Awake);
  const queueRunning = useUploadQueueStore((s) => s.running);
  const transferActive = useTransferStore(
    (s) => s.phase.kind === "starting" || s.phase.kind === "running",
  );
  const active = enabled && (queueRunning || transferActive);
  useEffect(() => {
    if (!active) return;
    const tickAll = () => {
      // Distinct hosts to keep awake: any console with a running queue item,
      // plus the currently-connected host (covers the single-shot upload).
      const hosts = new Set<string>();
      const conn = useConnectionStore.getState().host.trim();
      if (conn) hosts.add(conn);
      for (const it of useUploadQueueStore.getState().items) {
        if (it.status === "running") hosts.add(hostOf(it.addr));
      }
      for (const h of hosts) {
        void powerTick(mgmtAddr(h)).catch(() => {
          // best-effort — payload may be momentarily down during recovery
        });
      }
    };
    tickAll(); // reset the timer immediately when an upload starts
    const id = window.setInterval(tickAll, KEEP_PS5_AWAKE_TICK_MS);
    return () => window.clearInterval(id);
  }, [active]);
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
      // (2.11.0) Use safeUnlisten — was bare try/catch inline. Upload
      // and InstallPackage already standardised on safeUnlisten;
      // AppShell was the lone holdout, and the global unhandled-
      // rejection handler we added in 2.7.1 only catches the listener-
      // table-race rejection AFTER it fires. Using safeUnlisten on the
      // immediate-unlisten path here keeps every drag-drop site
      // identical and prevents the next regression of forgetting it.
      if (cancelled) safeUnlisten(fn);
      else unlisten = fn;
    }).catch(() => { /* subscribe-time rejection: nothing to clean */ });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
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
const ANDROID_STORAGE_PROMPT_DISMISSED_KEY =
  "ps5upload.android_storage_prompt.dismissed.v1";

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

function AndroidStorageAccessBanner() {
  const tr = useTr();
  const [visible, setVisible] = useState(false);
  const [checking, setChecking] = useState(false);

  const markDismissed = () => {
    try {
      window.localStorage.setItem(ANDROID_STORAGE_PROMPT_DISMISSED_KEY, "1");
    } catch {
      // localStorage can be blocked; still dismiss for this session.
    }
    setVisible(false);
  };

  const checkAccess = async () => {
    if (!isAndroid()) return;
    setChecking(true);
    try {
      const granted = await localFs.accessGranted();
      setVisible(!granted);
    } catch {
      setVisible(false);
    } finally {
      setChecking(false);
    }
  };

  useEffect(() => {
    if (!isAndroid()) return;
    try {
      if (
        window.localStorage.getItem(ANDROID_STORAGE_PROMPT_DISMISSED_KEY) ===
        "1"
      ) {
        return;
      }
    } catch {
      // localStorage can be blocked; still run the permission check.
    }
    void checkAccess();
  }, []);

  useEffect(() => {
    if (!visible || !isAndroid()) return;
    const onFocus = () => void checkAccess();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [visible]);

  if (!visible) return null;

  return (
    <div className="border-b border-[var(--color-border)] bg-[var(--color-warn-soft)] px-3 py-2 text-[var(--color-text)]">
      <div className="mx-auto flex max-w-6xl items-center gap-3">
        <Lock size={18} className="shrink-0 text-[var(--color-warn)]" />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">
            {tr(
              "android_storage_prompt_title",
              undefined,
              "Allow file access for Android uploads",
            )}
          </p>
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "android_storage_prompt_body",
              undefined,
              "Grant All files access so PS5Upload can upload game folders, .zip dumps, and .pkg files from your phone.",
            )}
          </p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Button
            variant="primary"
            size="sm"
            onClick={() =>
              void localFs
                .requestAccess()
                .catch(() => {})
                .finally(() => {
                  window.setTimeout(() => void checkAccess(), 750);
                })
            }
          >
            {tr("picker_open_settings", undefined, "Open settings")}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            loading={checking}
            leftIcon={<RefreshCw size={14} />}
            onClick={() => void checkAccess()}
          >
            {tr("picker_retry", undefined, "Retry")}
          </Button>
          <button
            type="button"
            aria-label={tr("dismiss", undefined, "Dismiss")}
            className="rounded p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
            onClick={markDismissed}
          >
            <X size={16} />
          </button>
        </div>
      </div>
    </div>
  );
}

export default function AppShell() {
  useStatusPolling();
  useUpdateCheckOnMount();
  useKeepPs5AwakeDuringUploads();
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
        void powerTick(mgmtAddr(host.trim())).catch(() => {
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
  // Mobile nav drawer open/closed. Only consulted below the md
  // breakpoint; on desktop the sidebar is always inline.
  const [mobileNavOpen, setMobileNavOpen] = useState(false);
  const tr = useTr();

  // Request OS notification permission once at startup (unless the user
  // disabled the mirror), so the macOS prompt / Android 13+
  // POST_NOTIFICATIONS dialog appears up front rather than mid-transfer.
  useEffect(() => {
    if (useNotificationsStore.getState().osNotifyEnabled) {
      void ensureOsNotificationPermission();
    }
  }, []);

  return (
    <div className="flex h-full flex-col bg-[var(--color-surface)] text-[var(--color-text)]">
      {/* Global in-app file/folder picker (Android real-path browser).
          Mounted once; screens drive it via pickLocalPath(). */}
      <LocalPathPicker />
      {/* Mobile top bar — only below the md breakpoint, where the fixed
          240px sidebar would otherwise eat most of a phone screen. The
          hamburger opens the sidebar as a slide-in drawer. */}
      <div className="flex items-center gap-2 border-b border-[var(--color-border)] bg-[var(--color-surface-2)] px-3 pb-2 pt-[calc(env(safe-area-inset-top)_+_0.5rem)] md:hidden">
        <button
          type="button"
          aria-label={tr("nav_open_aria", "Open navigation")}
          onClick={() => setMobileNavOpen(true)}
          className="rounded-md p-2 text-[var(--color-text)] hover:bg-[var(--color-surface-3)]"
        >
          <Menu size={22} />
        </button>
        <img
          src="/logo-square.png"
          alt="PS5Upload"
          className="h-8 w-8 rounded-md"
        />
        <span className="text-base font-bold tracking-tight">PS5Upload</span>
      </div>
      <AndroidStorageAccessBanner />

      <div className="flex min-h-0 flex-1">
        {/* Desktop: inline sidebar. Hidden on phones — the drawer below
            takes over so content gets the full width. */}
        <div className="hidden md:flex">
          <Sidebar />
        </div>

        {/* Mobile drawer: scrim + slide-in sidebar. The scrim and any nav
            item (via Sidebar's onNavigate) close it. */}
        {mobileNavOpen && (
          <div className="fixed inset-0 z-50 md:hidden">
            <button
              type="button"
              aria-label={tr("nav_close_aria", "Close navigation")}
              onClick={() => setMobileNavOpen(false)}
              className="absolute inset-0 bg-black/50"
            />
            <div className="absolute inset-y-0 left-0 flex max-w-[85%] shadow-xl">
              <Sidebar onNavigate={() => setMobileNavOpen(false)} />
            </div>
          </div>
        )}

        <main className="flex min-w-0 flex-1 flex-col overflow-hidden">
          <div className="flex-1 overflow-auto">
            <Outlet />
          </div>
        </main>
      </div>
      <ActivityBar />
      <StatusBar />
      <CommandPalette />
      <ShortcutsOverlay />
    </div>
  );
}
