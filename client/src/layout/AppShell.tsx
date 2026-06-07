import { Outlet, useLocation, useNavigate } from "react-router-dom";
import { useEffect, useRef, useState } from "react";
import { Lock, Menu, RefreshCw, X } from "lucide-react";
import Sidebar from "./Sidebar";
import StatusBar from "./StatusBar";
import ConsoleTabs from "./ConsoleTabs";
import ActivityBar from "./ActivityBar";
import { Button } from "../components/Button";
import { useConnectionStore, EMPTY_HOST_RUNTIME } from "../state/connection";
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
  const setStatus = useConnectionStore((s) => s.setStatus);
  const setHostStatus = useConnectionStore((s) => s.setHostStatus);
  const activeHost = useConnectionStore((s) => s.host);
  // Stable host-list key: ONLY the set of hosts (port-stripped, sorted), not
  // the full profile objects. The poller calls roster.noteSeen() on every
  // successful probe (updating last_seen_*), which replaces the profiles array;
  // depending on `profiles` here made that re-fire the effect → immediate
  // re-probe → noteSeen → … a payload_check STORM (dozens/sec) that exhausted
  // connections and knocked helpers offline (fatal with 2+ consoles). Keying
  // on just the host set means noteSeen no longer re-fires the poll.
  const hostsKey = useRosterStore((s) =>
    s.profiles
      .map((p) => (p.host ?? "").trim())
      .filter(Boolean)
      .sort()
      .join("|"),
  );
  const visible = useDocumentVisible();

  // Proactive health warnings — keyed PER HOST (the poll fans out over every
  // console), logged once per distinct condition so the bug bundle flags a
  // likely root cause (stale helper / no kernel R/W) before the user files.
  const appVersionRef = useRef<string | null>(null);
  const warnedMismatchRef = useRef<Record<string, string>>({});
  const warnedNoUcredRef = useRef<Record<string, boolean>>({});
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
    // FAN OUT: poll every known console (all roster profiles + the active
    // host), so each tab shows live status — not just the active one.
    // setHostStatus keys results by host and mirrors the active console to the
    // flat fields the screens read, so the old per-host carryover/stale-host
    // machinery is no longer needed (each host owns its own slot).
    const hosts = Array.from(
      new Set(
        [...hostsKey.split("|"), ...(activeHost ? [activeHost] : [])]
          .map((h) => (h ?? "").trim())
          .filter((h) => h.length > 0),
      ),
    );
    if (hosts.length === 0) {
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
    let cancelled = false;
    const isActive = (key: string) =>
      key === (hostOf(useConnectionStore.getState().host) || "_");
    const probeOne = async (probedHost: string) => {
      const key = hostOf(probedHost) || "_";
      try {
        const s = await payloadCheck(probedHost);
        if (cancelled) return;
        const prev =
          useConnectionStore.getState().runtimeByHost[key] ??
          EMPTY_HOST_RUNTIME;
        // Transient miss: keep this host's last-known version/kernel/ucred
        // rather than blanking the UI on a single dropped poll.
        const carryOver = !s.reachable;
        const newStatus = s.reachable ? "up" : "down";
        // Log only on an up<->down TRANSITION (not every poll).
        if (prev.payloadStatus !== "unknown" && prev.payloadStatus !== newStatus) {
          if (newStatus === "down")
            log.warn("connection", `helper went DOWN on ${probedHost}`);
          else log.info("connection", `helper came UP on ${probedHost}`);
        }
        setHostStatus(probedHost, {
          payloadStatus: newStatus,
          payloadVersion: carryOver ? prev.payloadVersion : s.payloadVersion,
          ps5Kernel: carryOver ? prev.ps5Kernel : s.ps5Kernel,
          ucredElevated: carryOver ? prev.ucredElevated : s.ucredElevated,
          maxTransferStreams: carryOver
            ? prev.maxTransferStreams
            : s.maxTransferStreams,
        });
        // Clear the active console's "rechecking…" flag once its probe lands.
        if (isActive(key)) setStatus({ payloadProbing: false });
        if (s.reachable) {
          // Update the matching roster row's cached firmware/payload.
          const roster = useRosterStore.getState();
          const prof = roster.profiles.find(
            (p) => (hostOf(p.host) || "_") === key,
          );
          if (prof)
            roster.noteSeen(prof.id, {
              kernel: s.ps5Kernel,
              payload: s.payloadVersion,
            });
          // Health: stale helper (per host).
          const av = appVersionRef.current;
          if (av && s.payloadVersion && s.payloadVersion !== av) {
            const k = `${key}:${s.payloadVersion}`;
            if (warnedMismatchRef.current[key] !== k) {
              warnedMismatchRef.current[key] = k;
              log.warn(
                "health",
                `helper version ${s.payloadVersion} != app ${av} on ${probedHost} — reload the payload (Connection → Replace)`,
              );
            }
          }
          // Health: no kernel R/W (per host); reset when it returns.
          if (s.ucredElevated === false) {
            if (!warnedNoUcredRef.current[key]) {
              warnedNoUcredRef.current[key] = true;
              log.warn(
                "health",
                `kernel R/W unavailable on ${probedHost} (kstuff not loaded) — some install/launch features degraded`,
              );
            }
          } else if (s.ucredElevated === true) {
            warnedNoUcredRef.current[key] = false;
          }
        }
      } catch (e) {
        if (cancelled) return;
        const prev =
          useConnectionStore.getState().runtimeByHost[key] ??
          EMPTY_HOST_RUNTIME;
        if (prev.payloadStatus === "up") {
          log.warn(
            "connection",
            `helper went DOWN on ${probedHost} (probe error: ${e instanceof Error ? e.message : String(e)})`,
          );
        }
        setHostStatus(probedHost, { payloadStatus: "down" });
        if (isActive(key)) setStatus({ payloadProbing: false });
      }
    };
    const tick = () => {
      for (const h of hosts) void probeOne(h);
    };
    tick();
    const h = setInterval(tick, 10000);
    return () => {
      cancelled = true;
      clearInterval(h);
    };
  }, [hostsKey, activeHost, setHostStatus, setStatus, visible]);
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
  const transferActive = useTransferStore((s) =>
    Object.values(s.phasesByHost).some(
      (p) => p.kind === "starting" || p.kind === "running",
    ),
  );
  const active = enabled && (queueRunning || transferActive);
  useEffect(() => {
    if (!active) return;
    const tickAll = () => {
      // Distinct hosts to keep awake: any console with a running queue item,
      // plus any console with a one-shot upload in flight. The single-shot set
      // is derived from the per-console phasesByHost (NOT the active tab) so an
      // upload on console A keeps A awake even after the user switches to tab B
      // — otherwise A could drop to rest mid-upload (the spool_apply_failed bug
      // this exists to prevent).
      const hosts = new Set<string>();
      for (const [h, p] of Object.entries(
        useTransferStore.getState().phasesByHost,
      )) {
        if ((p.kind === "starting" || p.kind === "running") && h) {
          hosts.add(hostOf(h));
        }
      }
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
      {/* Stack on phones (text block over buttons); single row on sm+. The
          old single-row layout squished the text into a narrow column on a
          phone because the button group is shrink-0. */}
      <div className="mx-auto flex max-w-6xl flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <Lock size={18} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
          <div className="min-w-0">
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
        </div>
        <div className="flex flex-wrap items-center gap-2 sm:shrink-0">
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
          {/* Console tab strip — one tab per PS5; switches the viewed console
              while every console's uploads/installs keep running in their own
              background loops. Hidden for single-console users. */}
          <ConsoleTabs />
          {/* Vertical scroll only. overflow-x-hidden is a backstop: the
              index.css width safety net makes content fit, but this guarantees
              the page can never scroll sideways. Nested blocks that are meant
              to scroll horizontally (tables in overflow-x-auto, code) have
              their own scroll context and are unaffected. */}
          <div className="flex-1 overflow-y-auto overflow-x-hidden">
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
