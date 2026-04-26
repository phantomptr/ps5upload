import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { getVersion } from "@tauri-apps/api/app";
import {
  useConnectionStore,
  PS5_LOADER_PORT,
} from "../../state/connection";
import {
  portCheck,
  payloadCheck,
  sendPayload,
  bundledPayloadPath,
  bundledPayloadInfo,
  probeCompanions,
  type BundledPayloadInfo,
  type CompanionStatus,
} from "../../api/ps5";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { pollUntilReady, type PollHandle } from "../../lib/pollUntilReady";
import { parsePS5Firmware } from "../../lib/ps5Firmware";
import { compareVersions } from "../../lib/semver";
import {
  AlertTriangle,
  CheckCircle2,
  CircleDashed,
  XCircle,
  Loader2,
  Send,
  ArrowRight,
  Plug,
} from "lucide-react";
import { PageHeader, Button } from "../../components";
import { useTr } from "../../state/lang";

type StepState = "idle" | "busy" | "ok" | "fail";

/**
 * Three-step guided flow before first upload:
 *
 *   1. Enter PS5 IP, probe the loader port (:9021).
 *   2. Send the payload ELF; poll the payload port (:9113) until it
 *      answers so we know the payload actually booted.
 *   3. Unlock "Go to Upload".
 *
 * Progressive disclosure: each step only renders once its predecessor
 * is `ok`. That keeps the screen focused on "what do I do next?"
 * rather than a checklist where three dim cards compete for attention.
 *
 * Step 2's Send button is the critical UX path — it stays disabled
 * for the ENTIRE send → probe → ready cycle (typically 3-20s), with
 * a live elapsed counter so the user can see progress instead of
 * wondering if their click did anything.
 */

/** Pick a button label that matches the current send phase so the user
 *  sees explicit progress in the thing they just clicked. Takes the
 *  active `tr` translator so the labels render in the user's locale —
 *  this is a pure helper but its outputs are user-facing. */
function sendButtonLabel(
  state: StepState,
  msg: string,
  elapsedMs: number,
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string,
  ) => string,
): string {
  if (state !== "busy") {
    return state === "ok"
      ? tr("connection_send_resend", undefined, "Resend payload")
      : tr("connection_send_send", undefined, "Send payload");
  }
  const sec = Math.max(1, Math.round(elapsedMs / 1000));
  const m = (msg || "").toLowerCase();
  if (m.includes("locating"))
    return tr("connection_send_locating", { sec }, `Locating ELF… (${sec}s)`);
  if (m.includes("sending"))
    return tr("connection_send_sending", { sec }, `Sending to PS5… (${sec}s)`);
  if (m.includes("waiting"))
    return tr(
      "connection_send_waiting",
      { sec },
      `Waiting for payload… (${sec}s)`,
    );
  return tr("connection_send_working", { sec }, `Working… (${sec}s)`);
}

function StepIcon({ state }: { state: StepState }) {
  const size = 22;
  if (state === "ok")
    return <CheckCircle2 size={size} className="text-[var(--color-good)]" />;
  if (state === "fail")
    return <XCircle size={size} className="text-[var(--color-bad)]" />;
  if (state === "busy")
    return (
      <Loader2
        size={size}
        className="animate-spin text-[var(--color-accent)]"
      />
    );
  return <CircleDashed size={size} className="text-[var(--color-muted)]" />;
}

function StepCard({
  index,
  title,
  state,
  stateText,
  children,
}: {
  index: number;
  title: string;
  state: StepState;
  stateText: string;
  children: React.ReactNode;
}) {
  const borderClass =
    state === "ok"
      ? "border-[var(--color-good)]"
      : state === "fail"
        ? "border-[var(--color-bad)]"
        : state === "busy"
          ? "border-[var(--color-accent)]"
          : "border-[var(--color-border)]";
  return (
    <section
      className={`rounded-lg border bg-[var(--color-surface-2)] p-5 transition-colors ${borderClass}`}
    >
      <header className="mb-4 flex items-center gap-3">
        <div className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-[var(--color-surface-3)] text-xs font-semibold tabular-nums">
          {index}
        </div>
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">{title}</div>
          <div className="mt-0.5 flex items-center gap-1.5 text-xs text-[var(--color-muted)]">
            <StepIcon state={state} />
            <span className="truncate">{stateText}</span>
          </div>
        </div>
      </header>
      <div>{children}</div>
    </section>
  );
}

export default function ConnectionScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const setHost = useConnectionStore((s) => s.setHost);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const payloadStatusHost = useConnectionStore((s) => s.payloadStatusHost);
  const storedStep1 = useConnectionStore((s) => s.step1);
  const storedStep1Msg = useConnectionStore((s) => s.step1Msg);
  const storedStep2 = useConnectionStore((s) => s.step2);
  const storedStep2Msg = useConnectionStore((s) => s.step2Msg);
  const setStoredStep1 = useConnectionStore((s) => s.setStep1);
  const setStoredStep2 = useConnectionStore((s) => s.setStep2);
  const navigate = useNavigate();

  const [transientStep1, setTransientStep1] = useState<StepState | null>(null);
  const [transientStep1Msg, setTransientStep1Msg] = useState<string | null>(
    null,
  );
  const [transientStep2, setTransientStep2] = useState<StepState | null>(null);
  const [transientStep2Msg, setTransientStep2Msg] = useState<string | null>(
    null,
  );

  /** Start time for the current send cycle. Drives the "(Ns)" suffix
   *  in the Send button so the user has explicit feedback during the
   *  otherwise-silent ~3-20s probe window. null when not in progress. */
  const sendStartedAt = useRef<number | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);

  /** Active boot-probe poller. Held so the component-unmount effect can
   *  cancel it — otherwise a poll fires up to 20 s after unmount and
   *  calls `setTransientStep2(null)` on a dead component. That was
   *  harmless today (React 18 silently drops the update) but surfaces
   *  as a dev warning and leaves the stale transient state behind if
   *  the user navigates back before stored state caught up. */
  const pollHandle = useRef<PollHandle | null>(null);
  useEffect(() => {
    return () => {
      pollHandle.current?.cancel();
      pollHandle.current = null;
    };
  }, []);

  const step1: StepState = transientStep1 ?? storedStep1;
  const step1Msg = transientStep1Msg ?? storedStep1Msg;
  const step2: StepState = transientStep2 ?? storedStep2;
  const step2Msg = transientStep2Msg ?? storedStep2Msg;
  const step3: StepState = step2 === "ok" ? "ok" : "idle";

  // 250ms tick while send/probe is busy — gives a smooth counter
  // without hammering React re-renders. Clears on settle.
  useEffect(() => {
    if (step2 !== "busy") {
      sendStartedAt.current = null;
      setElapsedMs(0);
      return;
    }
    if (sendStartedAt.current === null) {
      sendStartedAt.current = Date.now();
    }
    const id = window.setInterval(() => {
      if (sendStartedAt.current !== null) {
        setElapsedMs(Date.now() - sendStartedAt.current);
      }
    }, 250);
    return () => window.clearInterval(id);
  }, [step2]);

  // Auto-heal from the app's background status poller. When the
  // payloadStatus flips to "up" mid-flow (e.g., handleSend's own poll
  // hasn't resolved yet but the status bar saw the payload come up),
  // settle both stored AND transient so the UI actually transitions
  // to Step 3. Without clearing transient, step2 would resolve as
  // `transientStep2 ?? storedStep2` and the "Waiting for payload to
  // boot…" busy state would stay stuck even though stored went to
  // "ok" — user sees spinner forever while Send Payload from another
  // tab works just fine. Explicit settleStep2 handles both sides.
  useEffect(() => {
    // Skip if the recorded payloadStatus is for a different host than
    // the user is currently looking at — between the AppShell probe
    // firing and store-write, the user may have typed a new IP. We
    // don't want to label the new host with an "up" verdict that came
    // from the previous one.
    if (payloadStatusHost !== host) return;
    if (payloadStatus === "up") {
      if ((transientStep1 ?? storedStep1) !== "ok") {
        settleStep1("ok", tr(
          "connection_port_open",
          { port: PS5_LOADER_PORT, host },
          `Port ${PS5_LOADER_PORT} is open on ${host}`,
        ));
      }
      if ((transientStep2 ?? storedStep2) !== "ok") {
        settleStep2("ok", tr("connection_payload_running", { host }, `Payload is running on ${host}`));
      }
    } else if (payloadStatus === "down" && storedStep2 === "ok") {
      setStoredStep2("idle", tr("connection_payload_not_loaded", undefined, "Payload not loaded yet"));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [payloadStatus, payloadStatusHost, host]);

  const flashStep1 = (s: StepState, msg: string) => {
    setTransientStep1(s);
    setTransientStep1Msg(msg);
  };
  const settleStep1 = (s: "ok" | "fail" | "idle", msg: string) => {
    if (s === "ok") {
      setStoredStep1("ok", msg);
      setTransientStep1(null);
      setTransientStep1Msg(null);
    } else {
      setStoredStep1("idle", "Enter your PS5's address and check");
      setTransientStep1(s);
      setTransientStep1Msg(msg);
    }
  };
  const flashStep2 = (s: StepState, msg: string) => {
    setTransientStep2(s);
    setTransientStep2Msg(msg);
  };
  const settleStep2 = (s: "ok" | "fail" | "idle", msg: string) => {
    if (s === "ok") {
      setStoredStep2("ok", msg);
      setTransientStep2(null);
      setTransientStep2Msg(null);
    } else {
      setStoredStep2("idle", tr("connection_payload_not_loaded", undefined, "Payload not loaded yet"));
      setTransientStep2(s);
      setTransientStep2Msg(msg);
    }
  };

  async function handleCheck() {
    if (!host?.trim()) {
      settleStep1("fail", "Enter your PS5's IP address first.");
      return;
    }
    flashStep1("busy", `Checking ${host}:${PS5_LOADER_PORT}…`);
    settleStep2("idle", tr("connection_payload_not_loaded", undefined, "Payload not loaded yet"));
    setTransientStep2(null);
    setTransientStep2Msg(null);
    const ok = await portCheck(host, PS5_LOADER_PORT);
    if (ok) {
      settleStep1("ok", tr(
          "connection_port_open",
          { port: PS5_LOADER_PORT, host },
          `Port ${PS5_LOADER_PORT} is open on ${host}`,
        ));
    } else {
      settleStep1(
        "fail",
        `Port ${PS5_LOADER_PORT} is not open on ${host}`,
      );
    }
  }

  async function handleSend() {
    if (step1 !== "ok") return;
    if (step2 === "busy") return;
    flashStep2("busy", "Locating bundled payload ELF…");
    try {
      const elf = await bundledPayloadPath();
      flashStep2(
        "busy",
        `Sending ${elf} to ${host}:${PS5_LOADER_PORT}…`,
      );
      await sendPayload(host, elf);
    } catch (e) {
      settleStep2("fail", e instanceof Error ? e.message : String(e));
      return;
    }
    flashStep2("busy", "Waiting for payload to boot…");
    // Cancel any prior in-flight poll (e.g. user mashed Send twice)
    // before arming a new one — otherwise two polls race and both
    // eventually call settleStep2, flipping the visible state.
    pollHandle.current?.cancel();
    pollHandle.current = pollUntilReady({
      probe: async () => {
        const status = await payloadCheck(host);
        return status.reachable ? "ok" : "fail";
      },
      initialDelayMs: 1500,
      intervalMs: 1000,
      maxAttempts: 20,
      onResolved: (result) => {
        pollHandle.current = null;
        if (result === "ok") {
          settleStep2("ok", tr("connection_payload_running", { host }, `Payload is running on ${host}`));
        } else {
          settleStep2(
            "fail",
            "Payload didn't come up within 20s. If your PS5 is on, the ELF may have crashed — try sending again.",
          );
        }
      },
    });
  }

  return (
    <div className="p-6">
      <PageHeader
        icon={Plug}
        title={tr("connection_title", undefined, "Connect to your PS5")}
        description={tr(
          "connection_description",
          undefined,
          "Three quick steps before your first upload. You only need to do this once per PS5 boot — the payload stays loaded until the console reboots or goes into rest mode.",
        )}
      />

      <div className="mx-auto grid max-w-3xl gap-4">
        <StepCard
          index={1}
          title={tr("connection_step1_title", undefined, "Tell the app where your PS5 is")}
          state={step1}
          stateText={step1Msg}
        >
          <label className="block text-xs font-medium uppercase tracking-wide text-[var(--color-muted)]">
            {tr("ps5_address", undefined, "PS5 IP address")}
          </label>
          <div className="mt-2 flex items-center gap-2">
            <input
              value={host}
              onChange={(e) => {
                setHost(e.target.value);
                settleStep1("idle", "Enter your PS5's address and check");
                settleStep2("idle", tr("connection_payload_not_loaded", undefined, "Payload not loaded yet"));
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleCheck();
              }}
              placeholder="192.168.1.50"
              disabled={step2 === "busy"}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-50"
            />
            <Button
              variant="secondary"
              size="md"
              onClick={handleCheck}
              disabled={!host.trim() || step1 === "busy" || step2 === "busy"}
              loading={step1 === "busy"}
              title={tr(
                "connection_check_tooltip",
                { host: host || "the PS5", port: PS5_LOADER_PORT },
                `Probe ${host || "the PS5"}:${PS5_LOADER_PORT} to confirm it's reachable`,
              )}
            >
              {tr("connection_check", undefined, "Check")}
            </Button>
          </div>
          <p className="mt-3 text-xs text-[var(--color-muted)]">
            {tr(
              "connection_step1_hint",
              undefined,
              "Find this in the PS5's network settings, or on your router's device list. A wired Ethernet connection is strongly recommended over Wi-Fi.",
            )}
          </p>
        </StepCard>

        {step1 === "ok" && (
          <StepCard
            index={2}
            title={tr("connection_step2_title", undefined, "Send the payload to your PS5")}
            state={step2}
            stateText={step2Msg}
          >
            <p className="mb-4 text-sm text-[var(--color-muted)]">
              {tr(
                "connection_step2_hint",
                { port: PS5_LOADER_PORT },
                `The payload is a small program your PS5 runs in memory to accept uploads. Sent over port ${PS5_LOADER_PORT}; it takes a few seconds for the PS5 to respond once the bytes arrive.`,
              )}
            </p>
            <BundledPayloadBanner />
            <Button
              variant="primary"
              size="md"
              leftIcon={<Send size={14} />}
              onClick={handleSend}
              disabled={step2 === "busy"}
              loading={step2 === "busy"}
            >
              {sendButtonLabel(step2, step2Msg, elapsedMs, tr)}
            </Button>
            {step2 === "busy" && (
              <p className="mt-3 text-xs text-[var(--color-muted)]">
                {tr(
                  "connection_step2_busy_hint",
                  undefined,
                  "The PS5 typically boots the payload within 3-5 seconds. We keep polling for up to 20 seconds before giving up — if it times out, send it again.",
                )}
              </p>
            )}
          </StepCard>
        )}

        {step3 === "ok" && (
          <StepCard
            index={3}
            title={tr("connection_step3_title", undefined, "You're ready to upload")}
            state={step3}
            stateText={tr("connection_step3_ready", undefined, "PS5 is ready")}
          >
            <VersionBlock onResend={handleSend} />
            <p className="mb-4 text-sm leading-relaxed text-[var(--color-muted)]">
              {tr(
                "connection_step3_hint",
                undefined,
                "Go to the Upload tab and drop in a game folder, a .exfat image, or a .ffpkg image. For disk images, hit Mount in the Library tab. To register installed apps on your PS5 home screen, use a PS5-side installer (send it via the Send payload tab).",
              )}
            </p>
            <Button
              variant="primary"
              size="md"
              rightIcon={<ArrowRight size={14} />}
              onClick={() => navigate("/upload")}
            >
              {tr("connection_go_upload", undefined, "Go to Upload")}
            </Button>
          </StepCard>
        )}

        {host.trim() && <CompanionStrip host={host.trim()} />}
      </div>
    </div>
  );
}

/**
 * Small one-liner showing which ps5upload.elf is about to be sent —
 * path, size, and mtime. Cheap insurance against "I rebuilt but
 * Install is still gated" confusion: if the mtime in this banner is
 * older than your last `make payload`, the app is holding onto a
 * stale bundled resource and needs a rebuild/relaunch.
 *
 * In dev the payload comes from the repo's `payload/ps5upload.elf`
 * (so `make payload` refreshes it in place and the next Send picks
 * it up). In packaged builds it comes from the Tauri Resources dir
 * where the build-time copy lives.
 */
function BundledPayloadBanner() {
  const tr = useTr();
  const [info, setInfo] = useState<BundledPayloadInfo | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    bundledPayloadInfo()
      .then(setInfo)
      .catch((e) => setErr(e instanceof Error ? e.message : String(e)));
  }, []);
  if (err) {
    return (
      <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-[11px] text-[var(--color-bad)]">
        Can't find ps5upload.elf — run <code>make payload</code> first.
      </div>
    );
  }
  if (!info) return null;
  const kb = (info.size / 1024).toFixed(1);
  const built =
    info.mtime > 0
      ? new Date(info.mtime * 1000).toLocaleString()
      : "unknown";
  // Last path segment for compactness — the full path goes in the tooltip.
  const basename = info.path.split(/[\\/]/).pop() ?? info.path;
  return (
    <div
      className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-[11px] text-[var(--color-muted)]"
      title={info.path}
    >
      <span>
        <span className="font-semibold text-[var(--color-text)]">{basename}</span>{" "}
        · {kb} KB
      </span>
      <span>{tr("connection_built", { date: built }, `built: ${built}`)}</span>
    </div>
  );
}

/**
 * Compact scene-tool reachability strip. Renders one row below the
 * step cards once the user has entered a PS5 IP. Each chip is a TCP
 * connect-probe result — green dot when the port accepted, muted dot
 * otherwise. Clicking an alive chip opens the tool's homepage so
 * curious users can find out what it does.
 *
 * Rationale for living here (vs its own tab): these chips answer a
 * single practical question — "what PS5-side tools are running so I
 * know what I can do today?" — and that question is most relevant
 * right next to the rest of the connection context. The previous
 * dedicated Activity tab got cut for not paying for itself; this
 * one-line surface keeps the signal and drops the surface area.
 */
function CompanionStrip({ host }: { host: string }) {
  const tr = useTr();
  const [rows, setRows] = useState<CompanionStatus[] | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    let cancelled = false;
    // In-flight guard so the 30 s interval doesn't fire a second probe
    // while the first is still running. On a slow LAN the per-port TCP
    // probes can take 6 × 1.5 s = 9 s each; without this, a 30 s tick
    // that lands during a slow probe would start a concurrent probe
    // and both would race to setRows. Plain boolean is fine because
    // React effects run serially on a single thread.
    let inFlight = false;
    const run = async () => {
      if (inFlight) return;
      inFlight = true;
      setLoading(true);
      try {
        const r = await probeCompanions(host);
        if (!cancelled) setRows(r);
      } catch {
        if (!cancelled) setRows([]);
      } finally {
        inFlight = false;
        if (!cancelled) setLoading(false);
      }
    };
    run();
    // Re-probe every 30s while mounted. Connection-tab visits are
    // short, so a longer interval would miss "I just sent etaHEN, does
    // it show up?" — 30s is fast enough to feel responsive and slow
    // enough to not spam the LAN.
    const id = window.setInterval(run, 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [host]);

  // Only render tools that are actually answering right now — the
  // probe list itself is just the known-vocabulary of what we can
  // detect, not what "should" be installed. An empty strip means
  // "nothing loaded beyond ps5upload itself", which is a useful
  // signal by itself (no need to render grey-unreachable placeholders
  // the user can't act on).
  const liveRows = (rows ?? []).filter((r) => r.reachable);

  if (!rows) {
    return (
      <div className="rounded-md border border-dashed border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-xs text-[var(--color-muted)]">
        {loading
          ? tr("connection_scene_probing", undefined, "Probing scene tools…")
          : tr("connection_scene_idle", undefined, "Scene tools: —")}
      </div>
    );
  }

  return (
    <div className="rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2">
      <div className="mb-1.5 flex items-center justify-between text-[10px] font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        <span>
          {tr(
            "connection_scene_header",
            { host },
            `Scene tools on ${host}`,
          )}
        </span>
        {loading && (
          <span className="inline-flex items-center gap-1">
            <Loader2 size={10} className="animate-spin" />
            {tr("connection_scene_refreshing", undefined, "refreshing")}
          </span>
        )}
      </div>
      {liveRows.length === 0 ? (
        <div className="text-xs text-[var(--color-muted)]">
          {tr(
            "connection_scene_none",
            undefined,
            "None detected. Companion tools (etaHEN, ftpsrv) will appear here once they're loaded.",
          )}
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-3 text-xs">
          {liveRows.map((r) => (
            <CompanionPill key={r.port} row={r} />
          ))}
        </div>
      )}
    </div>
  );
}

const COMPANION_URLS: Record<string, string> = {
  etaHEN: "https://github.com/etaHEN/etaHEN/releases",
  ftpsrv: "https://github.com/ps5-payload-dev/ftpsrv/releases",
};

function CompanionPill({ row }: { row: CompanionStatus }) {
  const href = COMPANION_URLS[row.name];
  const tooltip = row.reachable
    ? `${row.role} — click to open ${row.name} on GitHub`
    : row.error ?? "not reachable";
  const onClick = () => {
    if (href) openExternal(href);
  };
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={!href}
      title={tooltip}
      className={
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 transition-colors " +
        (row.reachable
          ? "bg-[var(--color-good-soft)] text-[var(--color-good)] hover:bg-[var(--color-good)]/20"
          : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)]")
      }
    >
      <span
        className={
          "inline-block h-1.5 w-1.5 rounded-full " +
          (row.reachable
            ? "bg-[var(--color-good)]"
            : "bg-[var(--color-border)]")
        }
      />
      <span className="font-medium">{row.name}</span>
      <span className="font-mono text-[10px] opacity-70">:{row.port}</span>
    </button>
  );
}

/**
 * Renders the payload + PS5 firmware/kernel version once the payload is
 * up. Lives inside the "you're ready" card so it's visible when the user
 * is staring at a successful connection.
 *
 * Also compares the running payload version against the bundled
 * payload version. When the running payload is older than what this
 * app shipped (common after upgrading the desktop while the PS5
 * payload from a previous session is still loaded), shows an
 * actionable banner so the user knows to resend.
 */
function VersionBlock({ onResend }: { onResend?: () => void }) {
  const tr = useTr();
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const ps5Kernel = useConnectionStore((s) => s.ps5Kernel);
  const ps5Firmware = parsePS5Firmware(ps5Kernel);

  // Bundled-app version comes from Tauri at runtime — same value
  // `update-version.js` writes to package.json + payload's
  // config.h, so app-version === expected-payload-version by
  // construction.
  const [appVersion, setAppVersion] = useState<string | null>(null);
  useEffect(() => {
    getVersion()
      .then(setAppVersion)
      .catch((e) => {
        // Without an app version we can't compare against the
        // payload's reported version, so the outdated-payload
        // banner stays hidden. Surface why so a degraded Tauri
        // env doesn't silently disable a feature the user expects.
        console.warn(
          "[connection] getVersion failed; outdated-payload banner disabled:",
          e,
        );
        setAppVersion(null);
      });
  }, []);

  if (!payloadVersion && !ps5Kernel) return null;

  const cmp =
    payloadVersion && appVersion
      ? compareVersions(payloadVersion, appVersion)
      : null;
  // We only nudge on payload-older-than-app — newer payload (user
  // testing a future build, or a third-party signed payload that
  // happens to share our version table) is fine and silent.
  const payloadIsOlder = cmp === -1;

  return (
    <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3 text-xs">
      <div className="mb-2 font-medium uppercase tracking-wide text-[var(--color-muted)]">
        {tr("connection_block_connected", undefined, "Connected")}
      </div>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 font-mono">
        {payloadVersion && (
          <>
            <dt className="text-[var(--color-muted)]">
              {tr("connection_block_payload", undefined, "Payload")}
            </dt>
            <dd>
              v{payloadVersion}
              {appVersion && cmp === 0 && (
                <span className="ml-2 text-[10px] font-normal text-[var(--color-good)]">
                  {tr(
                    "connection_block_match",
                    undefined,
                    "matches this app",
                  )}
                </span>
              )}
              {appVersion && cmp === 1 && (
                <span className="ml-2 text-[10px] font-normal text-[var(--color-muted)]">
                  {tr(
                    "connection_block_newer",
                    { app: appVersion },
                    `newer than this app (v${appVersion})`,
                  )}
                </span>
              )}
            </dd>
          </>
        )}
        {ps5Firmware && (
          <>
            <dt className="text-[var(--color-muted)]">
              {tr("connection_ps5_firmware", undefined, "PS5 firmware")}
            </dt>
            <dd>{ps5Firmware}</dd>
          </>
        )}
        {ps5Kernel && (
          <>
            <dt className="text-[var(--color-muted)]">
              {tr("connection_kernel", undefined, "Kernel")}
            </dt>
            <dd className="break-all text-[var(--color-muted)]">
              {ps5Kernel}
            </dd>
          </>
        )}
      </dl>
      {ps5Kernel && !ps5Firmware && (
        <p className="mt-2 text-[11px] text-[var(--color-muted)]">
          Couldn't parse a firmware number from the kernel string. Look
          up the kernel ID on psdevwiki.com, or run{" "}
          <code className="rounded bg-[var(--color-surface-3)] px-1">
            ps5-versions.elf
          </code>{" "}
          from the Send payload tab.
        </p>
      )}

      {payloadIsOlder && appVersion && payloadVersion && (
        <div className="mt-3 flex flex-wrap items-start gap-2 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface-2)] p-2 text-[11px]">
          <AlertTriangle
            size={12}
            className="mt-0.5 shrink-0 text-[var(--color-warn)]"
          />
          <div className="min-w-0 flex-1">
            <div className="font-medium text-[var(--color-warn)]">
              {tr(
                "connection_payload_outdated_title",
                undefined,
                "PS5 has an older payload than this app",
              )}
            </div>
            <p className="mt-0.5 text-[var(--color-muted)]">
              {tr(
                "connection_payload_outdated_body",
                { running: payloadVersion, bundled: appVersion },
                `Running v${payloadVersion}, this app ships v${appVersion}. The bundled payload includes fixes the older one is missing — replace it for the best results.`,
              )}
            </p>
          </div>
          {onResend && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Send size={11} />}
              onClick={onResend}
            >
              {tr(
                "connection_payload_outdated_action",
                undefined,
                "Replace payload",
              )}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
