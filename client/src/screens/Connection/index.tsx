import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { getAppVersion } from "../../lib/appVersion";
import { isTauriEnv } from "../../lib/tauriEnv";
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
  discoverPs5,
  type BundledPayloadInfo,
  type CompanionStatus,
  type DiscoveredHost,
} from "../../api/ps5";
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
  Radar,
  Sparkles,
} from "lucide-react";
import { PageHeader, Button } from "../../components";
import { useTr } from "../../state/lang";
import PowerControl from "./PowerControl";
import { BringUpPanel } from "./BringUpPanel";

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
type SendPhase = "locating" | "sending" | "waiting";

function sendButtonLabel(
  state: StepState,
  // The explicit send phase, set by handleSend. Previously this re-parsed the
  // localized status MESSAGE with English substring matches (`includes("locating")`),
  // so under any non-English locale the message never matched and the button
  // was stuck on "Working…". Switching on a phase token localizes correctly.
  phase: SendPhase | null,
  elapsedMs: number,
  tr: (
    key: string,
    vars?: Record<string, string | number>,
    fallback?: string,
  ) => string,
): string {
  if (state !== "busy") {
    return state === "ok"
      ? tr("connection_send_resend", undefined, "Resend helper")
      : tr("connection_send_send", undefined, "Send helper");
  }
  const sec = Math.max(1, Math.round(elapsedMs / 1000));
  if (phase === "locating")
    return tr("connection_send_locating", { sec }, `Locating ELF… (${sec}s)`);
  if (phase === "sending")
    return tr("connection_send_sending", { sec }, `Sending to PS5… (${sec}s)`);
  if (phase === "waiting")
    return tr(
      "connection_send_waiting",
      { sec },
      `Waiting for helper… (${sec}s)`,
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
  const setStatus = useConnectionStore((s) => s.setStatus);
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
  // Explicit send phase for the Send button label — set by handleSend, cleared
  // on settle. Drives sendButtonLabel so the label localizes (it used to be
  // re-derived from the localized status message via English substring matches).
  const [sendPhase, setSendPhase] = useState<SendPhase | null>(null);

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
      // Cancelling the poll above stops any further writes from
      // handleSend's probe loop, but it doesn't undo the
      // payloadProbing=true that handleSend set when the user
      // clicked Replace. Without an explicit clear here, navigating
      // away mid-probe leaves the "rechecking…" badge latched in
      // the store until the next AppShell tick (up to 10 s).
      // Self-heals eventually but feels broken in the meantime —
      // unmount cleanup is the right place to reset.
      useConnectionStore.getState().setStatus({ payloadProbing: false });
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
        settleStep2("ok", tr("connection_payload_running", { host }, `Helper is running on ${host}`));
      }
    } else if (payloadStatus === "down" && storedStep2 === "ok") {
      setStoredStep2("idle", tr("connection_payload_not_loaded", undefined, "Helper not loaded yet"));
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
      setStoredStep1("idle", tr("connection_step1_idle", undefined, "Enter your PS5's address and check"));
      setTransientStep1(s);
      setTransientStep1Msg(msg);
    }
  };
  const flashStep2 = (s: StepState, msg: string) => {
    setTransientStep2(s);
    setTransientStep2Msg(msg);
  };
  const settleStep2 = (s: "ok" | "fail" | "idle", msg: string) => {
    setSendPhase(null);
    if (s === "ok") {
      setStoredStep2("ok", msg);
      setTransientStep2(null);
      setTransientStep2Msg(null);
    } else {
      setStoredStep2("idle", tr("connection_payload_not_loaded", undefined, "Helper not loaded yet"));
      setTransientStep2(s);
      setTransientStep2Msg(msg);
    }
  };

  async function handleCheck(overrideHost?: string) {
    // Optional `overrideHost` lets the discover panel auto-trigger a
    // check against the host the user just picked, without relying on
    // React state having re-rendered yet — calling handleCheck() with
    // no arg from a setTimeout right after setHost would close over
    // the *old* host value and probe the wrong address.
    const target = (overrideHost ?? host).trim();
    if (!target) {
      settleStep1(
        "fail",
        tr(
          "connection_enter_ip_first",
          undefined,
          "Enter your PS5's IP address first.",
        ),
      );
      return;
    }
    flashStep1(
      "busy",
      tr(
        "connection_checking",
        { target, port: PS5_LOADER_PORT },
        "Checking {target}:{port}…",
      ),
    );
    settleStep2("idle", tr("connection_payload_not_loaded", undefined, "Helper not loaded yet"));
    setTransientStep2(null);
    setTransientStep2Msg(null);
    const ok = await portCheck(target, PS5_LOADER_PORT);
    if (ok) {
      settleStep1("ok", tr(
          "connection_port_open",
          { port: PS5_LOADER_PORT, host: target },
          `Port ${PS5_LOADER_PORT} is open on ${target}`,
        ));
    } else {
      settleStep1(
        "fail",
        tr(
          "connection_port_closed",
          { port: PS5_LOADER_PORT, host: target },
          "Port {port} is not open on {host}",
        ),
      );
    }
  }

  async function handleSend() {
    const target = host.trim();
    if (step1 !== "ok") return;
    if (step2 === "busy") return;
    // Mark version + kernel as stale the moment the user clicks Send.
    // Two effects:
    //   1. The VersionBlock immediately renders with a "rechecking…"
    //      badge instead of letting the user squint at numbers from
    //      the *old* payload while the new one is still uploading.
    //   2. AppShell's 10 s-cadence poller doesn't owe us a fresh
    //      version anymore — handleSend's own probe loop (below) will
    //      flush both fields the moment payloadCheck succeeds, so the
    //      banner clears in lock-step with step2 going "ok" rather
    //      than waiting for the next AppShell tick.
    setStatus({
      payloadProbing: true,
      payloadVersion: null,
      ps5Kernel: null,
    });
    setSendPhase("locating");
    flashStep2(
      "busy",
      tr(
        "connection_locating_elf",
        undefined,
        "Locating bundled payload ELF…",
      ),
    );
    try {
      const elf = await bundledPayloadPath();
      setSendPhase("sending");
      flashStep2(
        "busy",
        tr(
          "connection_sending_elf",
          { elf, host: target, port: PS5_LOADER_PORT },
          "Sending {elf} to {host}:{port}…",
        ),
      );
      await sendPayload(target, elf);
    } catch (e) {
      // Send itself failed (loader-port unreachable, ELF missing,
      // etc.) — no new payload to probe; clear the probing flag so
      // VersionBlock stops showing the rechecking badge.
      setStatus({ payloadProbing: false });
      settleStep2("fail", e instanceof Error ? e.message : String(e));
      return;
    }
    setSendPhase("waiting");
    flashStep2(
      "busy",
      tr("connection_waiting_boot", undefined, "Waiting for payload to boot…"),
    );
    // Cancel any prior in-flight poll (e.g. user mashed Send twice)
    // before arming a new one — otherwise two polls race and both
    // eventually call settleStep2, flipping the visible state.
    pollHandle.current?.cancel();
    // Capture the last raw probe error so the timeout banner can
    // tell the user *why* the payload looks dead (kstuff not loaded,
    // mgmt port refused, etc.) rather than the generic "didn't come
    // up". Updated on every failed probe; surfaced in onResolved.
    let lastProbeError = "";
    pollHandle.current = pollUntilReady({
      probe: async () => {
        // payloadCheck returns reachability AND the new version /
        // kernel (from STATUS_ACK). The original code only consumed
        // `reachable` and discarded the rest, leaving the store with
        // the *old* version until the next AppShell 10 s tick — that's
        // the "old data was the latest" feel the user reported. By
        // writing version + kernel into the store the moment the new
        // payload answers, the VersionBlock flips to the new numbers
        // in lock-step with step2 going "ok".
        try {
          const status = await payloadCheck(target);
          if (status.reachable) {
            // Guard against a host change mid-flight. handleSend
            // captured `host` in its closure when the user clicked
            // Replace; if they typed a new IP into the input before
            // this probe resolved, the result we're holding is for
            // the OLD host. Don't pollute the store with it — let
            // AppShell's host-change effect handle the NEW host.
            if (useConnectionStore.getState().host === target) {
              setStatus({
                payloadStatus: "up",
                payloadStatusHost: target,
                payloadVersion: status.payloadVersion,
                ps5Kernel: status.ps5Kernel,
                ucredElevated: status.ucredElevated,
                maxTransferStreams: status.maxTransferStreams,
                payloadProbing: false,
              });
            }
            return "ok";
          }
          // Reachable=false: the engine returned 502 or similar.
          // Stash the engine's diagnostic for the timeout banner.
          if (status.error) lastProbeError = status.error;
          return "fail";
        } catch (e) {
          lastProbeError = e instanceof Error ? e.message : String(e);
          return "fail";
        }
      },
      initialDelayMs: 1500,
      intervalMs: 1000,
      maxAttempts: 20,
      onResolved: (result) => {
        pollHandle.current = null;
        if (result === "ok") {
          settleStep2("ok", tr("connection_payload_running", { host: target }, `Helper is running on ${target}`));
        } else {
          // Probe loop exhausted without a reachable payload. Clear
          // the rechecking flag so the banner doesn't dangle —
          // there's nothing further coming for it.
          setStatus({ payloadProbing: false });
          // Suggest the most common cause (no kstuff loaded yet) when
          // the symptom looks like "boot then immediate exit". Surface
          // the last raw probe error if any so the user has something
          // concrete to search for / report.
          const tail = lastProbeError
            ? ` Last probe: ${lastProbeError}.`
            : "";
          settleStep2(
            "fail",
            tr(
              "connection_payload_timeout",
              { tail },
              "Payload didn't come up within 20s.{tail} Just send it again — a fresh send now force-evicts any stuck previous instance on its own, so you usually don't need to restart the PS5. If it still fails: kstuff may not be loaded yet (run First Run, or send kstuff first), the ELF crashed on boot, or the PS5 is unreachable.",
            ),
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
          "Three quick steps before your first upload. You only need to do this once per PS5 boot — the helper stays loaded until the console reboots or goes into rest mode.",
        )}
      />

      <div className="mx-auto grid max-w-3xl gap-4">
        {/* First-run wizard nudge — shown only before the user has a
            running payload. Once step2 is "ok" they don't need it.
            One-click path; falls through to this manual flow if they
            prefer it. */}
        {step2 !== "ok" && (
          <div className="flex flex-wrap items-start gap-3 rounded-md border border-[var(--color-accent)] bg-[var(--color-surface-2)] p-3 text-xs">
            <Sparkles
              size={14}
              className="mt-0.5 shrink-0 text-[var(--color-accent)]"
            />
            {/* min-w floor (not 0): below ~14rem the text column was
                squeezing into a skinny tower beside the CTA on phones —
                the floor makes the button wrap underneath instead. */}
            <div className="min-w-[14rem] flex-1">
              <div className="font-semibold text-[var(--color-text)]">
                {tr(
                  "connection_first_run_nudge_title",
                  undefined,
                  "First time setting up?",
                )}
              </div>
              <div className="text-[var(--color-muted)]">
                {tr(
                  "connection_first_run_nudge_body",
                  undefined,
                  "The setup wizard installs kstuff + ShadowMount+ + ps5upload in one click. Or just step through the manual flow below.",
                )}
              </div>
            </div>
            <Button
              variant="primary"
              size="sm"
              rightIcon={<ArrowRight size={11} />}
              onClick={() => navigate("/first-run")}
            >
              {tr(
                "connection_first_run_nudge_action",
                undefined,
                "Open setup wizard",
              )}
            </Button>
          </div>
        )}
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
                settleStep1("idle", tr("connection_step1_idle", undefined, "Enter your PS5's address and check"));
                settleStep2("idle", tr("connection_payload_not_loaded", undefined, "Helper not loaded yet"));
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") void handleCheck();
              }}
              placeholder="192.168.1.50"
              disabled={step2 === "busy"}
              className="flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm outline-none focus:border-[var(--color-accent)] disabled:opacity-50"
            />
            <Button
              variant="secondary"
              size="md"
              onClick={() => void handleCheck()}
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
          <DiscoverPanel
            disabled={step2 === "busy"}
            currentHost={host}
            onPick={(picked) => {
              // Set the host, then auto-trigger handleCheck so the
              // user gets to step 2 in a single click rather than
              // (a) typing IP, (b) clicking Discover, (c) picking,
              // (d) clicking Check. The point of discovery is to
              // collapse those into one gesture.
              setHost(picked);
              settleStep1("idle", tr("connection_step1_idle", undefined, "Enter your PS5's address and check"));
              settleStep2("idle", tr("connection_payload_not_loaded", undefined, "Helper not loaded yet"));
              // Pass the picked host explicitly so we don't race
              // React's re-render — handleCheck's closure captures
              // the host that was current at last render.
              void handleCheck(picked);
            }}
          />
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
            title={tr("connection_step2_title", undefined, "Send the PS5Upload helper to your PS5")}
            state={step2}
            stateText={step2Msg}
          >
            <p className="mb-4 text-sm text-[var(--color-muted)]">
              {tr(
                "connection_step2_hint",
                { port: PS5_LOADER_PORT },
                `The PS5Upload helper is a small program your PS5 runs in memory to accept uploads. Sent over port ${PS5_LOADER_PORT}; it takes a few seconds for the PS5 to respond once the bytes arrive.`,
              )}
            </p>
            {isTauriEnv() ? (
              <>
                <BundledPayloadBanner />
                <Button
                  variant="primary"
                  size="md"
                  leftIcon={<Send size={14} />}
                  onClick={() => void handleSend()}
                  disabled={step2 === "busy"}
                  loading={step2 === "busy"}
                >
                  {sendButtonLabel(step2, sendPhase, elapsedMs, tr)}
                </Button>
                {step2 === "busy" && (
                  <p className="mt-3 text-xs text-[var(--color-muted)]">
                    {tr(
                      "connection_step2_busy_hint",
                      undefined,
                      "The PS5 typically boots the helper within 3-5 seconds. We keep polling for up to 20 seconds before giving up — if it times out, send it again.",
                    )}
                  </p>
                )}
              </>
            ) : (
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "connection_step2_browser_unsupported",
                  undefined,
                  "The browser can't read a local helper file to send. Load the helper from the desktop app or a USB autoloader first, then this page will detect it automatically.",
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
                "Go to the Upload tab and drop in a game folder, a .exfat image, or a .ffpkg image. For disk images, hit Mount in the Library tab. To register installed apps on your PS5 home screen, use a PS5-side installer (send it via the Payloads → Send file tab).",
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

        {/* Quick bring-up — the one-tap fast path that chains the bring-up
            playlist → helper → auto-loader. Shown once an address is set so a
            cold boot is one button instead of the manual step sequence above. */}
        {host.trim() && <BringUpPanel />}

        {/* Power control — only render once payload is up since the
            destructive actions need a working mgmt-port connection. */}
        {step2 === "ok" && <PowerControl host={host.trim()} />}
        {step2 === "ok" && <CompanionSuggestion />}
      </div>
    </div>
  );
}

/**
 * "Find PS5s on the network" — collapsed by default; user clicks the
 * Radar button to run a 3-second LAN scan via mDNS-SD + TCP probe.
 *
 * Design choices:
 *   - Inline panel (not a modal): keeps the user's eye on the IP
 *     input. Picking a row writes back to the same field they were
 *     about to type into, so visual continuity matters.
 *   - One-click "Use" per row: clicking auto-runs handleCheck against
 *     the picked host so the user goes from "I don't know my PS5's
 *     IP" to "Step 1 green" in two clicks total (Discover → Use).
 *   - Confidence badges (highly likely / possible / unknown): the
 *     backend is honest about uncertainty — "we found a host on your
 *     LAN that has the right ports open" is not the same as "we
 *     found a PS5." The badge tells the user how much to trust each
 *     row.
 *   - No automatic discovery on mount: a constant LAN browse would
 *     spam mDNS traffic on every Connection-tab visit. User-initiated
 *     keeps it explicit and quiet.
 */
function DiscoverPanel({
  disabled,
  currentHost,
  onPick,
}: {
  disabled: boolean;
  currentHost: string;
  onPick: (ip: string) => void;
}) {
  const tr = useTr();
  const [scanning, setScanning] = useState(false);
  const [hasScanned, setHasScanned] = useState(false);
  const [candidates, setCandidates] = useState<DiscoveredHost[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [scannedMs, setScannedMs] = useState(0);

  async function runScan() {
    setScanning(true);
    setError(null);
    try {
      const res = await discoverPs5();
      setCandidates(res.candidates);
      setScannedMs(res.scanned_ms);
      if (res.error) setError(res.error);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setCandidates([]);
    } finally {
      setScanning(false);
      setHasScanned(true);
    }
  }

  return (
    <div className="mt-2">
      <Button
        variant="ghost"
        size="sm"
        leftIcon={
          scanning ? (
            <Loader2 size={12} className="animate-spin" />
          ) : (
            <Radar size={12} />
          )
        }
        onClick={runScan}
        disabled={disabled || scanning}
      >
        {scanning
          ? tr("connection_discover_scanning", undefined, "Scanning LAN…")
          : hasScanned
            ? tr(
                "connection_discover_rescan",
                undefined,
                "Scan again",
              )
            : tr(
                "connection_discover_find",
                undefined,
                "Find PS5s on the network",
              )}
      </Button>
      {hasScanned && !scanning && (
        <DiscoverResults
          candidates={candidates}
          currentHost={currentHost}
          scannedMs={scannedMs}
          error={error}
          onPick={onPick}
        />
      )}
    </div>
  );
}

function DiscoverResults({
  candidates,
  currentHost,
  scannedMs,
  error,
  onPick,
}: {
  candidates: DiscoveredHost[];
  currentHost: string;
  scannedMs: number;
  error: string | null;
  onPick: (ip: string) => void;
}) {
  const tr = useTr();
  if (error) {
    return (
      <div className="mt-2 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
        {error}
      </div>
    );
  }
  if (candidates.length === 0) {
    return (
      <div className="mt-2 rounded-md border border-dashed border-[var(--color-border)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-muted)]">
        {tr(
          "connection_discover_empty",
          { ms: scannedMs },
          `No candidates found in ${scannedMs} ms. Make sure your PS5 is on the same LAN; try plugging it into Ethernet, or type the IP manually.`,
        )}
      </div>
    );
  }
  return (
    <div className="mt-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)]">
      <div className="border-b border-[var(--color-border)] px-3 py-1.5 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
        {tr(
          "connection_discover_header",
          { count: candidates.length, ms: scannedMs },
          `${candidates.length} candidate(s) — scanned in ${scannedMs} ms`,
        )}
      </div>
      <ul className="divide-y divide-[var(--color-border)]">
        {candidates.map((c) => (
          <DiscoverRow
            key={c.ip}
            row={c}
            isCurrent={c.ip === currentHost.trim()}
            onPick={onPick}
          />
        ))}
      </ul>
    </div>
  );
}

function DiscoverRow({
  row,
  isCurrent,
  onPick,
}: {
  row: DiscoveredHost;
  isCurrent: boolean;
  onPick: (ip: string) => void;
}) {
  const tr = useTr();
  // Three confidence buckets so the badge colour matches the user's
  // mental model: green = "yes, this is your PS5", amber = "looks
  // PS5-ish, give it a try", muted = "host showed up but no PS5
  // signals — probably not it".
  const tier =
    row.confidence >= 50 ? "high" : row.confidence >= 20 ? "mid" : "low";
  const tierColor =
    tier === "high"
      ? "bg-[var(--color-good-soft)] text-[var(--color-good)]"
      : tier === "mid"
        ? "bg-[var(--color-warn-soft)] text-[var(--color-warn)]"
        : "text-[var(--color-muted)]";
  const tierLabel =
    tier === "high"
      ? tr(
          "connection_discover_tier_high",
          undefined,
          "highly likely PS5",
        )
      : tier === "mid"
        ? tr("connection_discover_tier_mid", undefined, "possibly PS5")
        : tr("connection_discover_tier_low", undefined, "unknown");
  const badges: string[] = [];
  if (row.payload_port_open)
    badges.push(
      tr(
        "connection_discover_badge_payload",
        undefined,
        "ps5upload running",
      ),
    );
  if (row.loader_port_open)
    badges.push(
      tr(
        "connection_discover_badge_loader",
        undefined,
        "loader port open",
      ),
    );
  for (const svc of row.services) {
    // Strip the leading underscore + "_tcp" suffix for compactness:
    // "_sonic-loader._tcp" → "sonic-loader"
    const clean = svc.replace(/^_/, "").replace(/\._tcp$/, "");
    badges.push(clean);
  }
  return (
    <li className="flex items-center gap-3 px-3 py-2 text-xs">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-mono text-sm font-semibold">{row.ip}</span>
          {row.hostname && (
            <span className="truncate text-[var(--color-muted)]">
              {row.hostname}
            </span>
          )}
          <span
            className={`inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium ${tierColor}`}
            title={`confidence: ${row.confidence}/100`}
          >
            {tierLabel}
          </span>
        </div>
        {badges.length > 0 && (
          <div className="mt-1 flex flex-wrap gap-1.5 text-xs text-[var(--color-muted)]">
            {badges.map((b) => (
              <span
                key={b}
                className="rounded bg-[var(--color-surface-3)] px-1.5 py-0.5"
              >
                {b}
              </span>
            ))}
          </div>
        )}
      </div>
      <Button
        variant={isCurrent ? "ghost" : "secondary"}
        size="sm"
        onClick={() => onPick(row.ip)}
        disabled={isCurrent}
      >
        {isCurrent
          ? tr("connection_discover_current", undefined, "current")
          : tr("connection_discover_use", undefined, "Use")}
      </Button>
    </li>
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
    /* Show the backend's actual error string so the user knows WHY
     * extraction failed (mkdir EACCES, gunzip corruption, ELF magic
     * mismatch, disk full, …). The previous hardcoded "run make
     * payload first" message was misleading: the payload IS embedded
     * via include_bytes! at build time, so once the desktop shell
     * compiles, `make payload` no longer affects the running shell —
     * a fresh `tauri build` does. The most-likely real causes are:
     *   - desktop shell was built with an older payload (rebuild shell)
     *   - app_local_data_dir is not writable (permissions / sandbox)
     *   - disk full
     *   - gzip in the bundle is corrupted (very rare; fresh build fixes) */
    return (
      <div className="mb-3 rounded-md border border-[var(--color-bad)] bg-[var(--color-surface)] p-2 text-xs text-[var(--color-bad)]">
        <div className="font-semibold">
          {tr(
            "connection_bundled_payload_unavailable",
            "Bundled helper not available",
          )}
        </div>
        <div className="mt-0.5 break-words font-mono text-xs opacity-90">
          {err}
        </div>
        <div className="mt-1 text-xs opacity-80">
          {tr(
            "connection_rebuild_shell_hint",
            "If you've just rebuilt the helper, also rebuild the desktop shell (",
          )}
          <code>cargo build</code>{" "}
          {tr("connection_rebuild_shell_in", "in")}{" "}
          <code>client/src-tauri</code>
          {tr(
            "connection_rebuild_shell_embeds",
            ") so the new bytes get embedded. The shell embeds",
          )}{" "}
          <code>ps5upload.elf.gz</code>{" "}
          {tr("connection_rebuild_shell_compile_time", "at compile time.")}
        </div>
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
      className="mb-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-xs text-[var(--color-muted)]"
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
    // short, so a longer interval would miss "I just sent a loader, does
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
      <div className="mb-1.5 flex items-center justify-between text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
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
            "None detected. Companion tools will appear here once they're loaded.",
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

/**
 * Suggestion banner pointing the user at the Payloads tab to install
 * additional homebrew (kstuff, ShadowMount+, etc.). Dismissible —
 * remembers the user's "don't show again" choice via localStorage.
 *
 * Doesn't try to detect what's already installed (that would require
 * cross-coupling with the SMP detection panel, which lives on the
 * Library tab); just a discoverability nudge.
 */
function CompanionSuggestion() {
  const tr = useTr();
  const navigate = useNavigate();
  const [dismissed, setDismissed] = useState<boolean>(() => {
    if (typeof window === "undefined") return false;
    try {
      return window.localStorage.getItem("ps5upload.companion-suggestion.dismissed") === "1";
    } catch {
      return false;
    }
  });
  if (dismissed) return null;
  const dismiss = () => {
    try {
      window.localStorage.setItem("ps5upload.companion-suggestion.dismissed", "1");
    } catch {
      // best-effort persistence
    }
    setDismissed(true);
  };
  return (
    <div className="flex items-start gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3 text-xs">
      <Sparkles
        size={14}
        className="mt-0.5 shrink-0 text-[var(--color-accent)]"
      />
      <div className="min-w-0 flex-1">
        <div className="font-semibold">
          {tr(
            "companion_suggest_title",
            undefined,
            "Add more capabilities?",
          )}
        </div>
        <div className="text-[var(--color-muted)]">
          {tr(
            "companion_suggest_body",
            undefined,
            "ps5upload pairs with kstuff (kernel exploit), ShadowMount+ (auto-mount game backups), and a dozen other homebrew payloads. The Homebrew catalog tab installs them in one click.",
          )}
        </div>
      </div>
      <Button
        variant="primary"
        size="sm"
        rightIcon={<ArrowRight size={11} />}
        onClick={() => navigate("/payloads")}
      >
        {tr("companion_suggest_open", undefined, "Open library")}
      </Button>
      <button
        type="button"
        onClick={dismiss}
        className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface)] hover:text-[var(--color-text)]"
        title={tr("companion_suggest_dismiss", undefined, "Don't show again")}
      >
        ✕
      </button>
    </div>
  );
}

function CompanionPill({ row }: { row: CompanionStatus }) {
  const tooltip = row.reachable
    ? `${row.role} — listening on port ${row.port}`
    : row.error ?? "not reachable";
  return (
    <span
      title={tooltip}
      className={
        "inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 " +
        (row.reachable
          ? "bg-[var(--color-good-soft)] text-[var(--color-good)]"
          : "text-[var(--color-muted)]")
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
      <span className="font-mono text-xs opacity-70">:{row.port}</span>
    </span>
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
  const ucredElevated = useConnectionStore((s) => s.ucredElevated);
  const payloadProbing = useConnectionStore((s) => s.payloadProbing);
  const ps5Firmware = parsePS5Firmware(ps5Kernel);

  // Bundled-app version comes from Tauri at runtime — same value
  // `update-version.js` writes to package.json + payload's
  // config.h, so app-version === expected-payload-version by
  // construction.
  const [appVersion, setAppVersion] = useState<string | null>(null);
  useEffect(() => {
    getAppVersion()
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

  // Render the block during a probe even with no values yet — the
  // user just clicked Replace payload and seeing the section
  // disappear ("did the click do anything?") is more confusing than
  // seeing it with a "Probing…" placeholder.
  if (!payloadVersion && !ps5Kernel && !payloadProbing) return null;

  const cmp =
    payloadVersion && appVersion
      ? compareVersions(payloadVersion, appVersion)
      : null;
  // We only nudge on payload-older-than-app — newer payload (user
  // testing a future build, or a third-party signed payload that
  // happens to share our version table) is fine and silent.
  const payloadIsOlder = cmp === -1;

  // Dim numbers while a fresh probe is in flight so the user has a
  // clear "these may be stale" signal. Pairs with the Loader2 badge
  // in the header below — together they say "we know what's on
  // screen might be from before the new payload booted; we're
  // checking." Without this the user could read the OLD version
  // number and conclude the Replace didn't take.
  const valueClass = payloadProbing
    ? "text-[var(--color-muted)] italic"
    : "";

  return (
    <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-3 text-xs">
      <div className="mb-2 flex items-center gap-2 font-medium uppercase tracking-wide text-[var(--color-muted)]">
        <span>
          {tr("connection_block_connected", undefined, "Connected")}
        </span>
        {payloadProbing && (
          <span className="flex items-center gap-1 normal-case tracking-normal text-xs font-normal text-[var(--color-accent)]">
            <Loader2 size={10} className="animate-spin" />
            {tr(
              "connection_block_rechecking",
              undefined,
              "rechecking…",
            )}
          </span>
        )}
      </div>
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 font-mono">
        {payloadProbing && !payloadVersion && (
          <>
            <dt className="text-[var(--color-muted)]">
              {tr("connection_block_payload", undefined, "Helper")}
            </dt>
            <dd className="text-[var(--color-muted)] italic">
              {tr("connection_block_probing", undefined, "Probing…")}
            </dd>
          </>
        )}
        {payloadVersion && (
          <>
            <dt className="text-[var(--color-muted)]">
              {tr("connection_block_payload", undefined, "Helper")}
            </dt>
            <dd className={valueClass}>
              v{payloadVersion}
              {!payloadProbing && appVersion && cmp === 0 && (
                <span className="ml-2 text-xs font-normal text-[var(--color-good)]">
                  {tr(
                    "connection_block_match",
                    undefined,
                    "matches this app",
                  )}
                </span>
              )}
              {!payloadProbing && appVersion && cmp === 1 && (
                <span className="ml-2 text-xs font-normal text-[var(--color-muted)]">
                  {tr(
                    "connection_block_newer",
                    { app: appVersion },
                    "newer than this app (v{app})",
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
        {ucredElevated !== null && (
          <>
            <dt className="text-[var(--color-muted)]">
              {tr("connection_kernel_rw", undefined, "Kernel R/W")}
            </dt>
            <dd
              className={
                // While `payloadProbing` is true the value we're
                // showing is from BEFORE the new payload booted —
                // dim+italic to match how `payloadVersion` /
                // `ps5Kernel` get treated (see `valueClass`). Without
                // this, a green "available" pill could persist while
                // the user has just clicked Replace payload, falsely
                // suggesting the new payload elevated successfully.
                payloadProbing
                  ? "text-[var(--color-muted)] italic"
                  : ucredElevated
                    ? "text-[var(--color-good)]"
                    : "text-[var(--color-warn)]"
              }
            >
              {ucredElevated ? (
                tr(
                  "connection_kernel_rw_yes",
                  undefined,
                  "available — privileged install enabled",
                )
              ) : (
                <>
                  {tr(
                    "connection_kernel_rw_no",
                    undefined,
                    "not available — load via :9021 (jailbreak loader). Pkg install will fail without this.",
                  )}
                </>
              )}
            </dd>
          </>
        )}
      </dl>
      {ps5Kernel && !ps5Firmware && (
        <p className="mt-2 text-xs text-[var(--color-muted)]">
          {tr(
            "connection_firmware_parse_failed",
            "Couldn't parse a firmware number from the kernel string. Look up the kernel ID on psdevwiki.com, or run",
          )}{" "}
          <code className="rounded bg-[var(--color-surface-3)] px-1">
            ps5-versions.elf
          </code>{" "}
          {tr(
            "connection_firmware_from_send_tab",
            "from the Payloads → Send file tab.",
          )}
        </p>
      )}

      {payloadIsOlder && appVersion && payloadVersion && !payloadProbing && (
        <div className="mt-3 flex flex-wrap items-start gap-2 rounded-md border border-[var(--color-warn)] bg-[var(--color-surface-2)] p-2 text-xs">
          <AlertTriangle
            size={12}
            className="mt-0.5 shrink-0 text-[var(--color-warn)]"
          />
          <div className="min-w-0 flex-1">
            <div className="font-medium text-[var(--color-warn)]">
              {tr(
                "connection_payload_outdated_title",
                undefined,
                "PS5 has an older helper than this app",
              )}
            </div>
            <p className="mt-0.5 text-[var(--color-muted)]">
              {tr(
                "connection_payload_outdated_body",
                { running: payloadVersion, bundled: appVersion },
                `Running v${payloadVersion}, this app ships v${appVersion}. The bundled helper includes fixes the older one is missing — replace it for the best results.`,
              )}
            </p>
          </div>
          {onResend && isTauriEnv() && (
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<Send size={11} />}
              onClick={onResend}
            >
              {tr(
                "connection_payload_outdated_action",
                undefined,
                "Replace helper",
              )}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}
