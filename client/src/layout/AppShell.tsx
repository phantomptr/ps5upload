import { saveLastRoute } from "../lib/lastRoute";
import { useLocation, useNavigate, useOutlet } from "react-router";
import { Activity, useEffect, useRef, useState, type ReactNode } from "react";
import { Lock, RefreshCw, WifiOff, X } from "lucide-react";
import { localNetworkBlocked, requestLocalNetwork } from "../lib/androidLocalNetwork";
import StatusBar from "./StatusBar";
import SessionBanner from "./SessionBanner";
import EngineDownBanner from "./EngineDownBanner";
import { PairingDialog } from "../screens/Connection/PairingDialog";
import ConsoleTabs from "./ConsoleTabs";
import ActivityBar from "./ActivityBar";
import UpdateToast from "./UpdateToast";
import { Button } from "../components/Button";
import {
  useConnectionFrozen,
  useConnectionStore,
  EMPTY_HOST_RUNTIME,
  PS5_LOADER_PORT,
} from "../state/connection";
import { usePayloadPlaylistsStore } from "../state/payloadPlaylists";
import { log } from "../state/logs";
import { playlistResendsOurHelper } from "../lib/playlistOps";
import { useAutoSaveBackup } from "./useAutoSaveBackup";
import {
  autoRunStillLoaded,
  bootEpochMs,
  distinctPayloads,
  loadAutoRun,
  rememberAutoRun,
} from "../lib/autoLoaderGuard";
import { capturePayloadBlackBox } from "../lib/ps5Snapshot";
import {
  BLACK_BOX_SETTLE_MS,
  shouldCaptureBlackBox,
} from "../lib/payloadBlackBox";
import { useUpdateStore } from "../state/update";
import { engineApi } from "../api/engine";
import { fetchHwPower, payloadCheck, portCheck, procListGet } from "../api/ps5";
import { sessionNeedsAttention } from "../lib/consoleSession";
import { installActivityWiring } from "../state/activityWiring";
import { installTaskWiring } from "../state/taskWiring";
import {
  ensureRosterMigrated,
  useRosterStore,
  useActiveProfile,
} from "../state/roster";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauriEnv, safeUnlisten } from "../lib/tauriEnv";
import { engineIsOnThisDevice } from "../state/engine";
import { trStatic } from "../lib/trStatic";
import { useDocumentVisible } from "../lib/visibility";
import { useScheduleRunner } from "../state/schedules";
import {
  pushNotification,
  runNotificationAutoPrune,
  useNotificationsStore,
} from "../state/notifications";
import { ensureOsNotificationPermission } from "../lib/osNotify";
import { powerTick } from "../api/ps5";
import { anyAwakeWork, awakeConsoles } from "../lib/awakeWork";
import { useTaskStore } from "../state/tasks";
import { transferScreenBusy } from "../lib/ps5Transfers";
import {
  autoRedeployDecision,
  liveHelperDecision,
  MAX_REDEPLOYS_WITHOUT_RECOVERY,
} from "../lib/autoRedeploy";
import { CommandPalette } from "../components/CommandPalette";
import { ShortcutsOverlay } from "../components/ShortcutsOverlay";
import { LocalPathPicker } from "../components/LocalPathPicker";
import { Toaster } from "../components/Toaster";
import { LiveRegion } from "../components/LiveRegion";
import { SkipNav } from "../components/SkipNav";
import { TabBottomNav } from "./TabNav";
import Sidebar from "./Sidebar";
import NavigationControls from "./NavigationControls";
import { useWindowStatePersistence } from "../lib/windowState";
import {
  mgmtAddr,
  hostOf,
  PS5_AVA1_PORT,
} from "../lib/addr";
import { safeGetItem, safeSetItem } from "../lib/safeStorage";
import { useUploadQueueStore } from "../state/uploadQueue";
import {
  interruptedFsUploadHosts,
  resumeStoppedFsUploadOnWake,
} from "../state/fsUploadRuntime";
import { useTransferStore } from "../state/transfer";
import { useUploadSettingsStore } from "../state/uploadSettings";
import { ensurePayloadCurrent } from "../lib/ensurePayloadCurrent";
import { guardElfldr } from "../lib/elfldrGuard";
import { installPlayTimeAccumulator } from "../state/playTime";
import { installRunningWatch } from "../state/runningWatch";
import { useTr } from "../state/lang";
import { isAndroid } from "../lib/platform";
import { localFs } from "../api/localFs";
import { getAppVersion } from "../lib/appVersion";
import { GlobalPackageViewer } from "../components/GlobalPackageViewer";
import { dropTarget, usePackageViewer } from "../state/packageViewer";
import { installConvertRunner } from "../lib/convertQueueRunner";
import { recordAppEvent } from "../lib/appJournal";

// The Convert queue builds through the Convert pipeline wherever the user is in the app.
installConvertRunner();

/** Background status polling for the engine + payload dots in the
 *  status bar. Runs for the lifetime of the app so the indicators
 *  reflect current state regardless of which screen is visible.
 *
 *  - Engine: localhost `/api/jobs`, every 5s. Fast; doesn't touch PS5.
 *  - Payload: the PS5's :9120 via `payload_check`, every 10s, and only
 *    when a host is configured (no point spamming DOWN probes against
 *    the default IP if the user hasn't entered theirs). */
/** Consecutive failed payload probes required before the UI flips a host
 *  from "up" to "down". At a 10s poll interval, 2 means a real drop is
 *  reflected within ~20s, while a single jittery/busy poll is absorbed. */
const PROBE_MISS_THRESHOLD = 2;

/** Same debounce, but while an upload to that console is in flight.
 *
 *  A saturating transfer starves the console's network stack: the mgmt-port
 *  poll starts timing out, and at the normal threshold two consecutive
 *  timeouts (20 s) are enough to declare a perfectly healthy helper "down".
 *  A user bundle showed this happening repeatedly at ~150 MB/s — even :9021,
 *  served by the ELF loader rather than our helper, timed out under the same
 *  load. Missed polls during a big upload are expected, not evidence of a
 *  dead helper, so require a full minute of silence before believing it. */
const PROBE_MISS_THRESHOLD_DURING_TRANSFER = 6;

/** Minimum gap between auto-loader fires for the same console. The auto-run
 *  playlist sends ELFs to the loader, which briefly drops the helper (a
 *  down→up flap); this window swallows that self-induced edge so the
 *  auto-loader can't re-trigger itself. A genuine reconnect past the window
 *  fires again. Comfortably longer than a typical short playlist. */
const AUTO_LOADER_COOLDOWN_MS = 90_000;

/** How often to re-attempt a helper redeploy on a console that's been
 *  observed DOWN. The poll itself runs every 10s, but the redeploy send
 *  is gated to this cadence so we don't spam :9021 on a console that's
 *  genuinely in rest mode (where every connect would hang to its
 *  timeout). ~30s is quick enough to feel instant after a wake, slow
 *  enough to be cheap while the PS5 is asleep. */
const AUTO_REDEPLOY_INTERVAL_MS = 30_000;


/** What is running on a console and when it booted, or nulls when it cannot be asked. */
async function consoleBootAndProcesses(
  host: string,
): Promise<{ boot: number | null; names: string[] | null }> {
  const [power, procs] = await Promise.all([
    fetchHwPower(host).catch(() => null),
    procListGet(mgmtAddr(host)).catch(() => null),
  ]);
  const uptime = power?.operating_time_sec ?? 0;
  return {
    boot: uptime > 0 ? bootEpochMs(Date.now(), uptime) : null,
    names: procs?.ok ? procs.procs.map((p) => p.name) : null,
  };
}

/** How long after the playlist ends its payloads are looked for: one-shot payloads have
 *  exited by then, so only the ones that stay are remembered. */
const AUTO_LOADER_SETTLE_MS = 20_000;

/** Run the auto-loader's playlist on a console that just came up, unless this boot already
 *  ran it and what it loaded is still running (lib/autoLoaderGuard). */
async function runAutoLoaderOnce(
  key: string,
  host: string,
  playlistId: string,
  playlistName: string,
): Promise<void> {
  const before = await consoleBootAndProcesses(host);
  if (autoRunStillLoaded(loadAutoRun(key), before.boot, before.names)) {
    log.info(
      "connection",
      `auto-loader: not running "${playlistName}" on ${host} — it already ran since this console started and its payloads are still loaded`,
    );
    return;
  }
  log.info("connection", `auto-loader: running "${playlistName}" on ${host}`);
  await usePayloadPlaylistsStore.getState().run(playlistId, host, PS5_LOADER_PORT);
  await new Promise((r) => setTimeout(r, AUTO_LOADER_SETTLE_MS));
  const after = await consoleBootAndProcesses(host);
  if (after.boot !== null && after.names !== null) {
    rememberAutoRun(key, { bootEpochMs: after.boot, payloads: distinctPayloads(after.names) });
  }
}

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
  // Consecutive failed-probe counter per host. Used to debounce the up→down
  // transition: a single missed 10s poll (busy mgmt thread during a Library
  // scan, momentary network jitter) shouldn't flash "Helper isn't running"
  // when the helper is actually alive. Require N misses in a row first.
  const missCountRef = useRef<Record<string, number>>({});
  /** Per host: the last probe yielded no console verdict because OUR side
   *  failed — the engine didn't answer, or the IPC plumbing itself threw.
   *  Distinct from "the console is down". Kept so the transition is logged
   *  once instead of every 10s. */
  const blindProbeRef = useRef<Record<string, boolean>>({});
  /** Last `bytesSent` seen for each console's in-flight upload.
   *  Lets the poller use transfer progress as a liveness signal instead
   *  of competing with the very upload it is trying to monitor. */
  const transferProgressRef = useRef<Record<string, number>>({});
  // Auto-loader: last wall-clock ms we auto-ran the playlist for a host, used
  // to suppress re-triggering. The playlist itself sends ELFs to the loader,
  // which momentarily drops the helper (down→up flap) — without a cooldown
  // that flap would re-fire the auto-loader in a loop. One fire per cooldown
  // window per host; a genuine later reconnect (past the window) fires again.
  const autoLoaderFiredAtRef = useRef<Record<string, number>>({});
  // Wake-recovery upload resume: last ms we auto-resumed a host's failed
  // uploads on its down→up edge. Same cooldown discipline as the auto-loader
  // — a flapping helper must not loop-restart the queue.
  const uploadResumeFiredAtRef = useRef<Record<string, number>>({});
  // Load the saved queue at startup, not when a Queue panel first mounts:
  // installs from any screen go into it, and must never race the load.
  useEffect(() => {
    if (!useUploadQueueStore.getState().loaded) {
      void useUploadQueueStore.getState().hydrate();
    }
  }, []);
  // A Files upload the app closed or crashed in the middle of was saved: say so once, with
  // the way back to its Resume button.
  const trNotice = useTr();
  useEffect(() => {
    if (interruptedFsUploadHosts().length === 0) return;
    pushNotification(
      "info",
      trNotice(
        "fs_upload_interrupted_notice",
        undefined,
        "A copy to the PS5 was interrupted. Open Files to resume it.",
      ),
      { link: "/files" },
    );
    // Once, at startup.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    void getAppVersion()
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
        // Only the changes go to the journal: "unreachable" and "reachable again".
        const before = useConnectionStore.getState().engineStatus;
        if (up && before === "down")
          recordAppEvent({ cat: "app", level: "info", code: "engine_up", msg: "engine reachable again" });
        if (!up && before !== "down")
          recordAppEvent({ cat: "app", level: "error", code: "engine_down", msg: "engine unreachable" });
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
        priorInstance: null,
      });
      return;
    }
    let cancelled = false;
    // Prune per-host bookkeeping for consoles that left the roster, so these
    // refs don't accumulate dead keys across a long session of adding/removing
    // PS5s. Keyed the same way as the refs (hostOf-normalized).
    const liveKeys = new Set(hosts.map((h) => hostOf(h) || "_"));
    for (const ref of [
      autoLoaderFiredAtRef,
      missCountRef,
      transferProgressRef,
      warnedMismatchRef,
      warnedNoUcredRef,
    ]) {
      for (const k of Object.keys(ref.current)) {
        if (!liveKeys.has(k)) delete ref.current[k];
      }
    }
    const isActive = (key: string) =>
      key === (hostOf(useConnectionStore.getState().host) || "_");
    const probeOne = async (probedHost: string) => {
      const key = hostOf(probedHost) || "_";
      // ── Transfer progress IS the liveness signal ────────────────────
      //
      // While an upload to this console is moving bytes, polling it is both
      // redundant and harmful. Redundant because shards landing on :9120
      // prove the helper is alive far better than a probe does. Harmful
      // because a saturating upload starves the console's network stack —
      // a user bundle showed a 150 MB/s upload making even :9021 (the ELF
      // loader, a different program) time out — so the probe times out,
      // the helper is wrongly declared "down", and a redeploy lands on top
      // of a perfectly healthy transfer and kills it.
      //
      // So: if the byte count advanced since the last tick, mark the host
      // up and skip the probe entirely. That removes the probe's own
      // contention during exactly the window where the console can least
      // afford it. If the count has NOT advanced, the transfer may
      // genuinely be stuck, and we fall through to a real probe.
      if (transferScreenBusy(probedHost)) {
        const phase = useTransferStore.getState().phasesByHost[key];
        const sent = phase && phase.kind === "running" ? phase.bytesSent : null;
        const prevSent = transferProgressRef.current[key];
        // Any CHANGE counts as proof of life, not just an increase: a
        // resumed attempt restarts the byte count lower, and that still
        // means the console is receiving.
        if (sent !== null && sent !== prevSent) {
          transferProgressRef.current[key] = sent;
          missCountRef.current[key] = 0;
          setHostStatus(probedHost, { payloadStatus: "up" });
          if (isActive(key)) setStatus({ payloadProbing: false });
          return;
        }
      } else {
        delete transferProgressRef.current[key];
      }
      try {
        const s = await payloadCheck(probedHost);
        if (cancelled) return;
        const prev =
          useConnectionStore.getState().runtimeByHost[key] ??
          EMPTY_HOST_RUNTIME;
        // Transient miss: keep this host's last-known version/kernel/ucred
        // rather than blanking the UI on a single dropped poll.
        const carryOver = !s.reachable;
        // ── An engine outage is not a console outage ────────────────────
        //
        // Every console verdict comes from the engine: it is the only thing
        // that speaks STATUS to the console. So when the engine never
        // answered (dead, restarting, or too busy to reply in 5s), the
        // console's state is simply UNKNOWN — and a DOWN recorded here is
        // doubly dangerous. It is unretractable, because the next probe
        // needs the same engine that is missing, so nothing can ever flip
        // the host back to "up"; and it arms the auto-redeploy loop below,
        // which pushes a fresh ELF at :9021 every 30s. Each push kills the
        // running helper, so the console reads as down again — forever.
        //
        // That is exactly how both consoles were taken out on 2026-09-14: a
        // ~6 minute engine outage (self-inflicted, during a dev restart) put
        // 10 redeploys into each console (Phat :9021 1789424190→…478, Pro
        // 1789424255→…545 per /data/ps5upload/stderr.log) until they stopped
        // answering and had to be rebooted. Leave the last known status
        // alone and log the real culprit instead.
        if (carryOver && !s.engineReachable) {
          if (blindProbeRef.current[key] !== true) {
            blindProbeRef.current[key] = true;
            log.warn(
              "connection",
              `engine unreachable — ${probedHost}'s state is unknown, not down`,
            );
          }
          if (isActive(key)) setStatus({ payloadProbing: false });
          return;
        }
        if (blindProbeRef.current[key]) {
          blindProbeRef.current[key] = false;
          log.info("connection", `probes working again (${probedHost})`);
        }
        // Debounce up→down: a reachable probe clears the miss counter; an
        // unreachable one only flips to "down" after MISS_THRESHOLD misses in
        // a row, so one busy/jittery poll holds the last-known "up".
        let newStatus: "up" | "down";
        // A console that answers but wants pairing HAS a helper
        // running: calling it down would arm the auto-redeploy loop below against a live
        // console. Only the session state says what the person has to do about it.
        if (s.reachable || sessionNeedsAttention(s.session)) {
          missCountRef.current[key] = 0;
          newStatus = "up";
        } else {
          const misses = (missCountRef.current[key] ?? 0) + 1;
          missCountRef.current[key] = misses;
          const threshold = transferScreenBusy(probedHost)
            ? PROBE_MISS_THRESHOLD_DURING_TRANSFER
            : PROBE_MISS_THRESHOLD;
          newStatus =
            prev.payloadStatus === "up" && misses < threshold ? "up" : "down";
        }
        // Keep a copy of the helper's own logs every time it arrives at "up" —
        // including the first sighting after the app starts, which the
        // transition log below deliberately skips. A helper that dies soon
        // after starting is gone again before anyone can file a report, and
        // the report can only read logs through the helper; the new instance
        // can still read the files its predecessor left.
        //
        // Not at the instant it comes up, though. Reading then was followed,
        // every time, by the helper dropping mid-reply (FW 12.70 report,
        // 2026-09-23: up, first read reset 114 ms later, the port refused 27 ms
        // after that). Let it settle, and read only if it is still up.
        if (
          newStatus === "up" &&
          prev.payloadStatus !== "up" &&
          shouldCaptureBlackBox(probedHost, Date.now())
        ) {
          const captureHost = probedHost;
          window.setTimeout(() => {
            const now =
              useConnectionStore.getState().runtimeByHost[key];
            if (now?.payloadStatus !== "up") return;
            if (transferScreenBusy(captureHost)) return;
            void capturePayloadBlackBox(captureHost);
          }, BLACK_BOX_SETTLE_MS);
        }
        // Keep the patched elfldr in place (lib/elfldrGuard.ts) each time the helper is seen
        // arriving — including a helper already up when the app starts, which never goes
        // through ensurePayloadCurrent. Settled first, like the capture above, and never while
        // a transfer runs; guardElfldr itself asks at most every 10 minutes per console.
        if (newStatus === "up" && prev.payloadStatus !== "up") {
          const guardHost = probedHost;
          window.setTimeout(() => {
            const now = useConnectionStore.getState().runtimeByHost[key];
            if (now?.payloadStatus !== "up") return;
            if (transferScreenBusy(guardHost)) return;
            void guardElfldr(hostOf(guardHost) || guardHost, false);
          }, BLACK_BOX_SETTLE_MS);
        }
        // Log only on an up<->down TRANSITION (not every poll).
        if (
          prev.payloadStatus !== "unknown" &&
          prev.payloadStatus !== newStatus
        ) {
          if (newStatus === "down")
            log.warn("connection", `helper went DOWN on ${probedHost}`);
          else {
            log.info("connection", `helper came UP on ${probedHost}`);
            // Auto-loader: a genuine cold→ready edge on this console (the
            // `prev !== "unknown"` guard above means this does NOT fire when
            // the app launches and finds the helper already up — only after a
            // real reconnect or first-time-setup completion). Run the chosen
            // playlist against the console that just came up, once per
            // cooldown window so the playlist's own loader sends don't loop.
            const pl = usePayloadPlaylistsStore.getState();
            const cfg = pl.autoLoader;
            const auto = cfg.playlistId
              ? pl.playlists.find((p) => p.id === cfg.playlistId)
              : undefined;
            const firedAt = autoLoaderFiredAtRef.current[key] ?? 0;
            const onCooldown = Date.now() - firedAt < AUTO_LOADER_COOLDOWN_MS;
            // A playlist that re-sends OUR OWN helper cannot run here. This
            // edge fires the moment the helper reports UP, so sending it again
            // makes the new instance take over from the running one, shutting
            // that one down and dropping the connection — which produces
            // another edge, which fires this again. Refuse and say so, rather
            // than leaving someone chasing a connection that drops every time
            // they touch the app.
            const selfSend = auto
              ? playlistResendsOurHelper(auto.steps)
              : null;
            if (cfg.enabled && auto && auto.steps.length > 0 && !onCooldown && !selfSend) {
              autoLoaderFiredAtRef.current[key] = Date.now();
              void runAutoLoaderOnce(key, probedHost, auto.id, auto.name);
            } else if (cfg.enabled && selfSend) {
              log.warn(
                "connection",
                `auto-loader: not running "${auto?.name}" — step "${selfSend.path || selfSend.payloadId}" ` +
                  `sends ps5upload's own helper, which would take over the running one and drop this connection. ` +
                  `Remove that step; the helper is already up whenever this runs.`,
              );
            } else if (cfg.enabled) {
              // Enabled but didn't fire — record WHY, so "the auto-loader
              // didn't run" reports have a trace instead of silence.
              const why = !auto
                ? "no playlist selected"
                : auto.steps.length === 0
                  ? "playlist is empty"
                  : "within cooldown window";
              log.info(
                "connection",
                `auto-loader skipped on ${probedHost}: ${why}`,
              );
            }

            // Wake-recovery upload resume. The helper just came back (a real
            // reconnect edge — the `prev !== "unknown"` guard above excludes
            // app launch). A standby that outlasted the queue's own in-loop
            // recovery budget (3 attempts, ~2 min) left rows terminally
            // failed; re-drive the connection-class ones now that the console
            // answers again, so an overnight-interrupted upload finishes on
            // wake with no manual Retry. Gated by a per-host cooldown (a
            // flapping helper must not loop the queue) and skipped while a
            // transfer is live, matching the redeploy guard. The store no-ops
            // unless the `autoResume` setting is on.
            const resumedAt = uploadResumeFiredAtRef.current[key] ?? 0;
            const resumeCooling =
              Date.now() - resumedAt < AUTO_LOADER_COOLDOWN_MS;
            if (!resumeCooling && !transferScreenBusy(probedHost)) {
              uploadResumeFiredAtRef.current[key] = Date.now();
              // A Files-screen upload the outage stopped carries on the same way.
              if (resumeStoppedFsUploadOnWake(probedHost))
                log.info(
                  "connection",
                  `wake resume: carrying on with the Files upload on ${probedHost}`,
                );
              void useUploadQueueStore
                .getState()
                .resumeFailedRecoverable(probedHost)
                .then((n) => {
                  if (n > 0)
                    log.info(
                      "connection",
                      `wake resume: re-driving ${n} interrupted upload(s) on ${probedHost}`,
                    );
                });
            }
          }
        }
        setHostStatus(probedHost, {
          payloadStatus: newStatus,
          payloadVersion: carryOver ? prev.payloadVersion : s.payloadVersion,
          ps5Kernel: carryOver ? prev.ps5Kernel : s.ps5Kernel,
          ucredElevated: carryOver ? prev.ucredElevated : s.ucredElevated,
          priorInstance: carryOver ? prev.priorInstance : s.priorInstance,
          // The one probe's verdict. Never carried over: a stale "connected" would
          // hide a console that needs pairing.
          session: s.session,
        });
        // Clear the active console's "rechecking…" flag once its probe lands.
        if (isActive(key)) setStatus({ payloadProbing: false });
        // The install daemon (:9115) is no longer pre-armed or liveness-polled
        // from the client — the engine brings it up per-install during
        // `POST /api/pkg/install` and restores the main payload afterward.
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
        // The probe didn't complete — `payloadCheck` answers with a verdict
        // object in both the Tauri and the browser build, so an exception
        // here means the plumbing broke (IPC/serialization), not that the
        // console refused us. There is no evidence about the console, so
        // record none. This used to count as a miss and flip the host to
        // "down" after the threshold, which is how a broken engine turned
        // into an unrecoverable DOWN and a redeploy loop (2026-09-14).
        if (blindProbeRef.current[key] !== true) {
          blindProbeRef.current[key] = true;
          log.warn(
            "connection",
            `probe of ${probedHost} failed (${e instanceof Error ? e.message : String(e)}) — console state unchanged (${prev.payloadStatus}), not marking it down`,
          );
        }
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

/** Auto-redeploy the bundled helper to a console that's gone DOWN.
 *
 *  Why this exists: the PS5's payload is torn down every time the console
 *  rests (Sony kills userspace on suspend), and after a wake the helper is
 *  simply gone — no `down→up` edge fires to bring it back, because nothing
 *  re-sends it. The user had to click Connect → Send again after every rest,
 *  and in the meantime the fan threshold reset to default (the pinned value
 *  only takes effect once the helper is back and its watcher re-arms from
 *  fan_threshold.conf). Same after a network blip that dropped the TCP probe
 *  long enough to flip DOWN.
 *
 *  So while the app is open and a console we care about is DOWN, we
 *  periodically push the bundled ELF to :9021. While the PS5 is asleep the
 *  connect just fails (cheap); the moment it wakes, one of these sends lands,
 *  the helper boots, the boot-time fan restore re-applies the threshold, and
 *  the next poll sees it UP — at which point this loop skips it. No user
 *  action needed.
 *
 *  Brakes on the loop, because it pushes ELFs at a console that may be
 *  perfectly healthy. Every delivered ELF kills the running helper, so a
 *  deploy that fires when it shouldn't is not a no-op — it is the thing that
 *  breaks the console. Three of them in a row with no recovery is enough:
 *  the loop stops and reports. (2026-09-14: without that brake, a ~6 minute
 *  engine outage — which the poller misread as both consoles being down —
 *  sent 10 helpers to each console and left them unresponsive.)
 *
 *  - Native-only: a browser session has no bundled ELF to push.
 *  - Honors the `autoRedeployOnWake` setting (default ON).
 *  - One in-flight redeploy per host (no pile-up if a 9021 connect hangs).
 *  - Re-probes before sending: a console that answers is left alone, and an
 *    engine that can't be reached has no verdict to justify a send.
 *  - Gated to AUTO_REDEPLOY_INTERVAL_MS per host so a sleeping PS5 isn't
 *    bombarded with connect attempts.
 *  - Stops after MAX_REDEPLOYS_WITHOUT_RECOVERY delivered helpers with no
 *    "up" verdict in between; re-arms on any "up".
 *  - Pauses when the window is hidden (matches the poller). */
function useAutoRedeployDownHelpers() {
  const visible = useDocumentVisible();
  // Per-host bookkeeping: last wall-clock send attempt + whether a send is
  // currently in flight. Kept in refs so the interval closure always sees
  // fresh values without re-arming the effect.
  const lastAttemptRef = useRef<Record<string, number>>({});
  const inFlightRef = useRef<Record<string, boolean>>({});
  // Consecutive ELFs actually DELIVERED to :9021 without the helper ever
  // reporting back up. A failed send (console asleep, loader refused) does not
  // count — that's the rest-mode case this loop exists for, and it must keep
  // trying through an all-night standby. A delivered send that doesn't produce
  // an up verdict means we are the problem, not the console.
  const deliveredRef = useRef<Record<string, number>>({});
  // Consecutive ticks a console was left alone because its helper still
  // accepted connections (see liveHelperDecision).
  const liveHoldsRef = useRef<Record<string, number>>({});
  useEffect(() => {
    if (!visible) return;
    if (!isTauriEnv()) return;
    let cancelled = false;

    const tryRedeploy = async (host: string) => {
      const key = hostOf(host) || host;
      if (inFlightRef.current[key]) return;
      const now = Date.now();
      if (now - (lastAttemptRef.current[key] ?? 0) < AUTO_REDEPLOY_INTERVAL_MS)
        return;
      lastAttemptRef.current[key] = now;
      inFlightRef.current[key] = true;
      try {
        // ── Price of admission: a fresh verdict that the console is gone ──
        //
        // The "down" that woke this loop was read up to 15s ago and can be
        // stale by now (a helper that finished booting, a console that just
        // woke). Pushing on a stale verdict is not harmless — the send kills
        // the running helper — so ask the console right now, and only send if
        // the ENGINE answered and said the console did not. An engine that
        // can't be reached holds the send: it has no verdict to give, and its
        // absence is not evidence about the console.
        let fresh: Awaited<ReturnType<typeof payloadCheck>>;
        try {
          fresh = await payloadCheck(host);
        } catch {
          // No verdict at all. Hold; the poller will try again.
          return;
        }
        if (cancelled) return;
        if (fresh.reachable || sessionNeedsAttention(fresh.session)) {
          log.info(
            "connection",
            `auto-redeploy: ${host} answered a fresh probe — nothing to restore (the down verdict was stale)`,
          );
          return;
        }
        if (!fresh.engineReachable) {
          log.warn(
            "connection",
            `auto-redeploy: holding off on ${host} — the engine is unreachable, so it cannot say whether the console needs a helper`,
          );
          return;
        }
        // ── A helper that still owns our ports is alive ─────────────────
        //
        // STATUS can fail while the helper runs (one listener down, a busy
        // accept loop). A push then starts a new instance that takes over
        // from the live one and drops the connection again. Plain TCP
        // connects only, never an RPC.
        const portsOpen = (
          await Promise.all(
            [PS5_AVA1_PORT].map((port) =>
              portCheck(hostOf(host) || host, port).catch(() => false),
            ),
          )
        ).some(Boolean);
        if (cancelled) return;
        const held = liveHoldsRef.current[key] ?? 0;
        if (liveHelperDecision({ portsOpen, held }) === "hold") {
          liveHoldsRef.current[key] = held + 1;
          if (held === 0) {
            log.warn(
              "connection",
              `auto-redeploy: holding off on ${host} — the helper is not answering STATUS, ` +
                `but its ports still accept connections, so it is running. Sending another ` +
                `would replace it and drop the connection.`,
            );
          }
          return;
        }
        liveHoldsRef.current[key] = 0;
        // ensurePayloadCurrent(force=true) probes, then sends + polls up to
        // ~30s. While the PS5 is in rest mode the probe/send fails fast; once
        // awake it lands and boots. Either way it never throws.
        log.info(
          "connection",
          `auto-redeploy: attempting helper restore on ${host}`,
        );
        const result = await ensurePayloadCurrent(host, () => cancelled, true);
        // "pushed"/"stale-ok" both mean an ELF reached the loader. If a
        // console takes MAX_REDEPLOYS_WITHOUT_RECOVERY of them and still
        // reads down, pushing more cannot help — it only keeps killing
        // whatever helper manages to start. Stop and say so, loudly enough
        // to find in a bug report. Resets the moment the console reports up.
        if (result === "pushed" || result === "stale-ok") {
          const n = (deliveredRef.current[key] ?? 0) + 1;
          deliveredRef.current[key] = n;
          if (n >= MAX_REDEPLOYS_WITHOUT_RECOVERY) {
            log.error(
              "connection",
              `auto-redeploy: gave up on ${host} after ${n} delivered helpers with no recovery — ` +
                `the console is not coming back on its own. Check it (power, rest mode, network), ` +
                `then use Connection → Send. Not sending more.`,
            );
          }
        }
      } finally {
        inFlightRef.current[key] = false;
      }
    };

    const tick = () => {
      if (cancelled) return;
      if (!useUploadSettingsStore.getState().autoRedeployOnWake) return;
      const { runtimeByHost } = useConnectionStore.getState();
      const roster = useRosterStore.getState();
      for (const p of roster.profiles) {
        const h = (p.host ?? "").trim();
        if (!h) continue;
        const key = hostOf(h) || h;
        // All four guards live in the predicate so they can be pinned by a
        // test (see lib/autoRedeploy.ts): never while a transfer is live,
        // never on a console whose state we never learned, never past the
        // brake, and re-arm on any "up". The transfer guard in particular is
        // load-bearing — a saturating upload once made the mgmt poll time
        // out, the helper was declared "down", the redeploy landed and killed
        // the transfer mid-flight, and the whole thing repeated: 33 redeploys,
        // 23 helper shutdowns, 132 GB re-sent for a 20 GB archive.
        const decision = autoRedeployDecision({
          status: runtimeByHost[key]?.payloadStatus,
          busy: transferScreenBusy(h),
          delivered: deliveredRef.current[key] ?? 0,
        });
        if (decision === "rearm") {
          liveHoldsRef.current[key] = 0;
          if ((deliveredRef.current[key] ?? 0) > 0) {
            deliveredRef.current[key] = 0;
            log.info("connection", `auto-redeploy: ${h} is back up — re-armed`);
          }
        } else if (decision === "redeploy") {
          void tryRedeploy(h);
        }
      }
    };
    // Stagger the first tick off the poller so the redeploy check reads the
    // poller's freshest DOWN verdict (poller runs at t=0 and every 10s; we
    // run at t=15s and every 15s — interleaved rather than aligned).
    const firstId = window.setTimeout(tick, 15_000);
    const id = window.setInterval(tick, 15_000);
    // Browser online event: when the network comes back (WiFi reconnects,
    // laptop wakes from sleep and NIC reassociates), skip the wait and try
    // a redeploy right now for any console we'd been trying to reach. This
    // is the "I switched networks and had to re-run the app" complaint —
    // now it recovers within ~one connect timeout instead of up to 15s.
    const onOnline = () => {
      if (cancelled) return;
      log.info("connection", "network online event — immediate redeploy sweep");
      tick();
    };
    window.addEventListener("online", onOnline);
    return () => {
      cancelled = true;
      window.clearTimeout(firstId);
      window.clearInterval(id);
      window.removeEventListener("online", onOnline);
    };
  }, [visible]);
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

/** Keep the PS5 awake by deferring its auto-standby timer
 *  (sceSystemServicePowerTick — resets the IDLE timer only; manual rest
 *  from the controller still works). Three policies (Settings → Upload):
 *
 *    "transfers" (default) — tick every console with a running upload, install or copy,
 *      every few minutes. The PS5's shortest auto-standby setting
 *      (~20 min) would otherwise drop a long upload into rest mode
 *      mid-transfer — the `spool_apply_failed` failure.
 *    "always" — additionally tick EVERY console whose helper is up, for
 *      as long as the app is open. The console never auto-rests while
 *      you're working with it.
 *    "off" — never tick.
 *
 *  Each tick is one tiny mgmt frame; failures (helper momentarily down
 *  during auto-resume) are ignored. The 2-minute cadence gives ten
 *  chances per shortest-rest-window, so a couple of dropped frames
 *  can't let the console slip away.
 */
const KEEP_PS5_AWAKE_TICK_MS = 2 * 60 * 1000;
function useKeepPs5Awake() {
  const mode = useUploadSettingsStore((s) => s.keepPs5AwakeMode);
  const queueRunning = useUploadQueueStore((s) => s.running);
  // Plain loop instead of Object.values().some(): this selector runs on
  // EVERY transfer-store write (multiple per second during an upload),
  // and allocating a fresh values array each time is avoidable GC churn
  // for what boils down to a boolean.
  const transferActive = useTransferStore((s) => {
    for (const h in s.phasesByHost) {
      const p = s.phasesByHost[h];
      if (p.kind === "starting" || p.kind === "running") return true;
    }
    return false;
  });
  // Installs and copies count as transfers too (they register tasks).
  const taskWork = useTaskStore((s) => anyAwakeWork(s.tasks));
  const active =
    mode === "always" ||
    (mode === "transfers" && (queueRunning || transferActive || taskWork));
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
      // Installs (stream, link, archive, upload) and console copies too, not only uploads.
      for (const h of awakeConsoles(useTaskStore.getState().tasks)) hosts.add(hostOf(h));
      if (useUploadSettingsStore.getState().keepPs5AwakeMode === "always") {
        // Every console whose helper currently answers — read live at tick
        // time (not effect deps) so consoles joining/leaving are picked up
        // on the next tick without restarting the interval.
        for (const [h, rt] of Object.entries(
          useConnectionStore.getState().runtimeByHost,
        )) {
          if (rt.payloadStatus === "up" && h && h !== "_") hosts.add(h);
        }
      }
      for (const h of hosts) {
        void powerTick(mgmtAddr(h)).catch(() => {
          // best-effort — payload may be momentarily down during recovery
        });
      }
    };
    tickAll(); // reset the timer immediately when the policy activates
    const id = window.setInterval(tickAll, KEEP_PS5_AWAKE_TICK_MS);
    return () => window.clearInterval(id);
  }, [active]);
}

/** App-wide drag-drop listener: a package, game image or folder dropped on a screen without
 *  its own drop zone opens the package viewer, whose Install… / Convert… takes it to the screen
 *  that does that. A drop never starts anything by itself. Screens with their own drop zone
 *  (Install Package, Payloads, Upload, Convert) keep their drops. */
function usePkgAutoRoute() {
  const navigate = useNavigate();
  const location = useLocation();
  useEffect(() => {
    if (!isTauriEnv()) return; // browser-only dev/test contexts skip Tauri APIs
    if (isAndroid()) return; // no drag-and-drop on Android — skip the bridge round-trip
    // A dropped file is a path on this device; a remote engine can't open it.
    if (!engineIsOnThisDevice()) return;
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    const p = getCurrentWebview().onDragDropEvent((e) => {
      if (cancelled) return;
      if (e.payload.type !== "drop") return;
      // The first viewable thing ANYWHERE in the drop, not just paths[0]: a mixed drop like
      // [game.elf, patch.pkg] still finds the package.
      const target = dropTarget(e.payload.paths ?? [], location.pathname);
      if (!target) return;
      const viewer = usePackageViewer.getState();
      viewer.open(target.path, [
        target.kind === "package"
          ? {
              label: trStatic("drop_install", "Install…"),
              primary: true,
              onClick: () => {
                viewer.close();
                navigate("/install-package", { state: { droppedPath: target.path } });
              },
            }
          : {
              label: trStatic("drop_convert", "Convert…"),
              primary: true,
              onClick: () => {
                viewer.close();
                navigate("/convert", { state: { source: target.path } });
              },
            },
      ]);
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
    }).catch(() => {
      /* subscribe-time rejection: nothing to clean */
    });
    return () => {
      cancelled = true;
      if (unlisten) safeUnlisten(unlisten);
    };
  }, [navigate, location.pathname]);
}

/** Save the current screen so the app reopens there (see lib/lastRoute —
 *  the landing redirect does the reopening). Debounced so back/forward
 *  chains don't write per click. */
const ANDROID_STORAGE_PROMPT_DISMISSED_KEY =
  "ps5upload.android_storage_prompt.dismissed.v1";

function useRoutePersistence() {
  const location = useLocation();

  // Includes a workspace tab encoded in the query string (for example
  // /games?tab=files): restoring only the pathname made every workspace
  // reopen its default view and discarded the user's place.
  useEffect(() => {
    if (typeof window === "undefined") return;
    const id = window.setTimeout(() => {
      try {
        saveLastRoute(location.pathname, location.search);
      } catch {
        // best-effort
      }
    }, 500);
    return () => window.clearTimeout(id);
  }, [location.pathname, location.search]);
}

/**
 * Stale-helper banner.
 *
 * When the app updates but the PS5 still runs the previous payload, the two
 * halves disagree about what exists — features land in the app that the
 * console can't serve, and fixes look like they haven't worked. This was
 * logged as a warning and NOTHING else, so a user could run a 2½-version-old
 * app against a newer helper and have no way to see it: they simply saw
 * behaviour that didn't match the release notes and reasonably concluded the
 * fix was broken. A log line nobody opens is not user-visible.
 *
 * Dismissible per version pair, so re-loading the payload isn't nagged about
 * forever, but a NEW mismatch (next update) surfaces again.
 */
function HelperVersionBanner() {
  const tr = useTr();
  const navigate = useNavigate();
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(() =>
    safeGetItem("ps5upload.helper-mismatch.dismissed"),
  );
  useEffect(() => {
    let cancelled = false;
    void getAppVersion()
      .then((v) => {
        if (!cancelled) setAppVersion(v);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const mismatch =
    payloadStatus === "up" &&
    !!appVersion &&
    !!payloadVersion &&
    payloadVersion !== appVersion;
  const pair = `${payloadVersion}->${appVersion}`;
  if (!mismatch || dismissed === pair) return null;

  return (
    <div className="border-b border-[var(--color-border)] bg-[var(--color-warn-soft)] px-3 py-2 text-[var(--color-text)]">
      <div className="mx-auto flex max-w-6xl flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <RefreshCw
            size={18}
            className="mt-0.5 shrink-0 text-[var(--color-warn)]"
          />
          <div className="min-w-0">
            <p className="text-sm font-medium">
              {tr(
                "helper_mismatch_title",
                { payload: payloadVersion ?? "?", app: appVersion ?? "?" },
                `The PS5 helper is v${payloadVersion} but this app is v${appVersion}`,
              )}
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "helper_mismatch_body",
                undefined,
                "Fixes and features from the newer version won't work until the helper on the console matches. Reload it from the Connection screen.",
              )}
            </p>
          </div>
        </div>
        <div className="flex shrink-0 gap-2">
          <Button
            size="sm"
            variant="primary"
            onClick={() => navigate("/connection")}
          >
            {tr("helper_mismatch_go", undefined, "Reload helper")}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              safeSetItem("ps5upload.helper-mismatch.dismissed", pair);
              setDismissed(pair);
            }}
          >
            {tr("dismiss", undefined, "Dismiss")}
          </Button>
        </div>
      </div>
    </div>
  );
}

function AndroidStorageAccessBanner() {
  const tr = useTr();
  const [visible, setVisible] = useState(false);
  const [checking, setChecking] = useState(false);

  const markDismissed = () => {
    try {
      safeSetItem(ANDROID_STORAGE_PROMPT_DISMISSED_KEY, "1");
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
      if (safeGetItem(ANDROID_STORAGE_PROMPT_DISMISSED_KEY) === "1") {
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
          <Lock
            size={18}
            className="mt-0.5 shrink-0 text-[var(--color-warn)]"
          />
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

/** Android 17 blocks the app from the local network until the user allows it, and
 *  then nothing can reach the PS5. Not dismissable: without it the app cannot work. */
function AndroidLocalNetworkBanner() {
  const tr = useTr();
  const [blocked, setBlocked] = useState(() => localNetworkBlocked());

  useEffect(() => {
    const recheck = () => setBlocked(localNetworkBlocked());
    recheck();
    window.addEventListener("focus", recheck);
    document.addEventListener("visibilitychange", recheck);
    // Android's prompt returns no event to the page; poll while it is open.
    const t = window.setInterval(recheck, 2000);
    return () => {
      window.removeEventListener("focus", recheck);
      document.removeEventListener("visibilitychange", recheck);
      window.clearInterval(t);
    };
  }, []);

  if (!blocked) return null;

  return (
    <div
      role="alert"
      className="border-b border-[var(--color-bad)] bg-[var(--color-bad-soft)] px-3 py-3 text-[var(--color-text)]"
    >
      <div className="mx-auto flex max-w-6xl flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <WifiOff size={20} className="mt-0.5 shrink-0 text-[var(--color-bad)]" />
          <div className="min-w-0">
            <p className="text-sm font-semibold">
              {tr(
                "android_local_network_title",
                undefined,
                "Allow PS5Upload on your local network",
              )}
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "android_local_network_body",
                undefined,
                "Android blocks the app from reaching your PS5 until you allow it.",
              )}
            </p>
          </div>
        </div>
        <Button variant="primary" size="sm" onClick={() => requestLocalNetwork()}>
          {tr("android_local_network_allow", undefined, "Allow")}
        </Button>
      </div>
    </div>
  );
}

export default function AppShell() {
  useStatusPolling();
  useAutoSaveBackup();
  useAutoRedeployDownHelpers();
  useUpdateCheckOnMount();
  useKeepPs5Awake();
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
    }
  });
  // Subscribe-once: wires the per-feature stores (transfer, FS bulk
  // op, FS download) into the cross-screen activity history. Safe to
  // call on every render because installActivityWiring is idempotent.
  useEffect(() => {
    installActivityWiring();
    // Subscribe-once: wires the same per-feature stores into the
    // unified Task store (v5 §10). Idempotent — safe to call repeatedly.
    installTaskWiring();
    // Migrate single-host users into the multi-PS5 roster on first
    // start. Idempotent — no-op when the roster is already populated.
    ensureRosterMigrated();
    // Subscribe-once: cross-store accumulator that credits running
    // titles with elapsed wall-clock between updates. Idempotent.
    installPlayTimeAccumulator();
    // One app-wide poll for "is a game running", so the Games badge works
    // from any screen — not just the two that used to keep the store fed.
    // Backs off while a screen-level loop is publishing. Idempotent.
    installRunningWatch();
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
  // Window title follows the active console so the macOS Dock / app
  // switcher / Windows taskbar answer "which PS5 am I looking at?"
  // without bringing the window forward. Single-console users keep the
  // plain product name. document.title works in every Tauri webview
  // (and is a no-op-safe write on Android).
  const activeProfile = useActiveProfile();
  const multiConsoleTitle = useRosterStore((s) => s.profiles.length > 1);
  useEffect(() => {
    try {
      document.title =
        multiConsoleTitle && activeProfile
          ? `PS5Upload — ${activeProfile.name}`
          : "PS5Upload";
    } catch {
      // non-DOM test environments
    }
  }, [activeProfile, multiConsoleTitle]);

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
      {/* v5 global a11y infrastructure — mounted once at shell root. */}
      <SkipNav />
      <LiveRegion />
      {/* Global in-app file/folder picker (Android real-path browser).
          Mounted once; screens drive it via pickLocalPath(). */}
      <LocalPathPicker />
      <GlobalPackageViewer />
      {/* v5 mobile top bar — kept slim; primary nav is the bottom
          tab bar. Only renders below md. */}
      <div className="h-top-bar flex items-center gap-2 border-b border-[var(--color-border)] bg-[var(--color-surface-raised)] px-3 pb-2 pt-[calc(env(safe-area-inset-top)_+_0.5rem)] shadow-sm md:hidden">
        <NavigationControls />
        <img
          src={`${import.meta.env.BASE_URL.replace(/\/+$/, "")}/logo-square.png`}
          alt="PS5Upload"
          className="h-8 w-8 rounded-[0.6rem]"
        />
        <span className="text-base font-bold tracking-tight">PS5Upload</span>
      </div>
      <HelperVersionBanner />
      <SessionBanner />
      <EngineDownBanner />
      <AndroidLocalNetworkBanner />
      <AndroidStorageAccessBanner />

      <div className="flex min-h-0 flex-1">
        {/* Desktop defaults to labeled navigation again. Users who prefer the
            compact v5 rail can collapse it explicitly; that choice persists. */}
        <Sidebar />

        <main
          id="main"
          tabIndex={-1}
          className="flex min-w-0 flex-1 flex-col overflow-hidden bg-[color-mix(in_oklab,var(--color-surface)_96%,var(--color-surface-2)_4%)] outline-none"
        >
          {/* Console tab strip — one tab per PS5; switches the viewed console
              while every console's uploads/installs keep running in their own
              background loops. Hidden for single-console users. */}
          <ConsoleTabs />
          {/* Slim, dismissible "update available" bar (the auto-check already
              ran on mount). Pinned above the scroll area so it stays visible. */}
          <UpdateToast />
          {/* Browser-style history for a desktop app: route and workspace-tab
              changes are first-class views, so users can retrace a workflow
              without reopening More or rebuilding a search. */}
          <div className="hidden h-10 items-center border-b border-[var(--color-border)] bg-[var(--color-surface-raised)] px-3 md:flex">
            <NavigationControls />
          </div>
          {/* Vertical scroll only. overflow-x-hidden is a backstop: the
              index.css width safety net makes content fit, but this guarantees
              the page can never scroll sideways. Nested blocks that are meant
              to scroll horizontally (tables in overflow-x-auto, code) have
              their own scroll context and are unaffected. */}
          {/* `key={pathname}` re-runs the entrance animation on navigation.
              Route changes already swap the rendered screen component, so
              re-creating this wrapper adds no extra remount cost — and it
              guarantees scroll position resets per screen. Same-path query
              changes (e.g. /payloads?tab=send) keep the node, so tab
              switches inside a screen don't re-animate. */}
          <KeptScreens pathname={location.pathname} className="anim-screen flex-1 overflow-y-auto overflow-x-hidden pb-[calc(56px+var(--safe-bottom))] md:pb-0 [overscroll-behavior:contain]" />
        </main>
      </div>
      <ActivityBar />
      <StatusBar />
      <CommandPalette />
      <ShortcutsOverlay />
      {/* v5 mobile bottom nav — labels stay visible and More is a full route. */}
      <TabBottomNav />
      {/* v5 Toaster — critical-toast overlay. Mounted last so it
          sits above all other chrome in DOM order. */}
      <Toaster />
      <PairingDialog />
    </div>
  );
}


/** How many screens stay alive behind the one on show. Enough for going back and forth in a
 *  workflow; bounded so a long session does not keep every screen it ever opened. */
const KEPT_SCREENS = 6;

/** The current screen, and the last few visited ones kept alive but hidden.
 *
 * A route change used to unmount the screen: anything typed, selected, opened or scrolled was
 * gone on return, and a screen-local operation lost its progress. Each visited screen now
 * stays mounted inside a hidden <Activity>, which keeps its state and DOM but tears its
 * effects down, so a hidden screen runs no timers and polls nothing: it costs the console no
 * traffic. Coming back re-runs its effects (lists refresh) with the state as it was left.
 *
 * Each screen has its own scroll container, so scroll position is per screen too. Only the
 * one on show carries `data-scroll-root` (see lib/useScrollLock).
 *
 * This covers switching screens. Switching console is covered one level up: App.tsx keeps a
 * whole tree of screens per console, and this component is inside each of them. */
function KeptScreens({ pathname, className }: { pathname: string; className: string }) {
  const outlet = useOutlet();
  const behindAnotherConsole = useConnectionFrozen();
  // path -> the route element as first rendered for it. The element's props do not change
  // for a given route, so the first one keeps rendering the same component instance.
  const [kept, setKept] = useState<Array<{ path: string; node: ReactNode }>>([]);
  useEffect(() => {
    setKept((prev) => {
      const here = prev.find((k) => k.path === pathname) ?? { path: pathname, node: outlet };
      // Most recent last; the oldest beyond the limit is let go.
      return [...prev.filter((k) => k.path !== pathname), here].slice(-KEPT_SCREENS);
    });
    // The outlet element is new on every render; only a change of screen matters here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname]);
  // The screen being opened is not in `kept` until the effect above has run.
  const screens = kept.some((k) => k.path === pathname)
    ? kept
    : [...kept, { path: pathname, node: outlet }];
  return (
    <>
      {[...screens]
        .sort((a, b) => a.path.localeCompare(b.path))
        .map(({ path, node }) => {
          const active = path === pathname;
          return (
            <Activity key={path} mode={active ? "visible" : "hidden"}>
              <div
                // Not in a console's tree that is itself hidden behind another console.
                {...(active && !behindAnotherConsole ? { "data-scroll-root": "" } : {})}
                data-screen={path}
                // Hidden screens are out of reach as well as out of sight: nothing in them
                // can take focus, be clicked, or be read out.
                inert={!active}
                aria-hidden={!active}
                className={className}
              >
                {/* The screen on show renders the live route element; a hidden one keeps
                    the element it was last shown with. Same component either way. */}
                {active ? outlet : node}
              </div>
            </Activity>
          );
        })}
    </>
  );
}
