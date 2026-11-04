import { useState } from "react";
import { Camera, Check, AlertTriangle, Images, MonitorSmartphone, Zap } from "lucide-react";
import { useNavigate } from "react-router";

import { useConnectionStore, EMPTY_HOST_RUNTIME } from "../state/connection";
import { parsePS5Firmware } from "../lib/ps5Firmware";
import { shellErrorText } from "../lib/shellError";
import { useTr } from "../state/lang";
import { captureAppScreenshot } from "../lib/captureScreenshot";
import { useCaptureViewer } from "../lib/appCaptures";
import { useToastStore } from "../state/toasts";
import { isTauriEnv } from "../lib/tauriEnv";
import {
  useRosterStore,
  useActiveProfile,
  profileAccentForHost,
} from "../state/roster";
import { ActivityStatusSlot } from "./ActivityBar";
import { useUploadSettingsStore } from "../state/uploadSettings";
import { getEngineUrl } from "../state/engine";
import { Spinner } from "../components/Spinner";
import { hostOf } from "../lib/addr";
import {
  consoleLight,
  consoleLightColor,
  type ConsoleLight,
} from "../lib/consolePower";
import { consoleLightLabel } from "../lib/consoleLightLabel";
import { deviceName, meshFor, othersOnline, useMeshStore } from "../state/mesh";

/**
 * App-footer status strip: engine + payload liveness, plus the versions
 * reported by the PS5 side when it's reachable.
 *
 * PS5 firmware display: we parse the user-visible firmware number (e.g.
 * "9.60") out of the kernel string for the primary display. The full
 * kernel build string ("FreeBSD 11.0 r218215/releases/09.60 …") stays
 * available as a tooltip. Before this change the footer truncated the
 * kernel string mid-word, hiding the firmware value users actually
 * want to see.
 */
export default function StatusBar() {
  const tr = useTr();
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  const engineError = useConnectionStore((s) => s.engineError);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const session = useConnectionStore((s) => s.session);
  const power = useConnectionStore((s) => s.power);
  const payloadVersion = useConnectionStore((s) => s.payloadVersion);
  const ps5Kernel = useConnectionStore((s) => s.ps5Kernel);
  const ps5Firmware = parsePS5Firmware(ps5Kernel);
  // Multi-console awareness. The flat payloadStatus above mirrors only
  // the ACTIVE tab, so a non-active console dropping mid-upload was
  // previously invisible until the user happened to switch tabs. Name
  // the active console on the helper pill and add one mini status dot
  // per OTHER console (fed by the fan-out poller's runtimeByHost).
  const profiles = useRosterStore((s) => s.profiles);
  const activeProfile = useActiveProfile();
  const runtimeByHost = useConnectionStore((s) => s.runtimeByHost);
  const multiConsole = profiles.length > 1;
  const otherConsoles = multiConsole
    ? profiles.filter((p) => p.id !== activeProfile?.id)
    : [];

  const dot = (light: ConsoleLight) => (
    <span
      data-light={light}
      className="inline-block h-2 w-2 rounded-full"
      style={{ background: consoleLightColor(light) }}
      aria-hidden
    />
  );
  // The dots are colour alone: say the state to a screen reader too.
  const dotText = (light: ConsoleLight) => (
    <span className="sr-only">{consoleLightLabel(light, tr)}</span>
  );
  const engineLight: ConsoleLight =
    engineStatus === "up" ? "up" : engineStatus === "down" ? "down" : "unknown";

  // The footer says two things, so it shows two groups: the Engine (the
  // app's own local backend) and the PS5 (the console we're talking to).
  // Everything else — helper version, kernel build, profile name — is
  // detail that belongs in a tooltip, not strung along the bar as a row
  // of cryptic pipe-separated tokens.
  const ps5Connected = payloadStatus === "up";
  // Console name: in a multi-console setup the active profile's name is
  // the clearest label ("Pro", "Fat"); otherwise just "PS5".
  const ps5Name =
    multiConsole && activeProfile
      ? activeProfile.name
      : tr("status_ps5", undefined, "PS5");
  // Pack the connection state, kernel build and helper version into one
  // hover string. Reads top-to-bottom: are we connected, what firmware,
  // what helper build.
  // The one probe's verdict: a console that answers but needs pairing is not
  // "connected" and not "down" either.
  // Rest mode reads as its own calm state, not as a dropped connection.
  const ps5Light = consoleLight({ payloadStatus, power, session });
  const ps5Tooltip = [
    ps5Light === "unknown"
      ? tr("status_ps5_disconnected", undefined, "Not connected")
      : consoleLightLabel(ps5Light, tr),
    ps5Kernel || null,
    payloadVersion
      ? tr(
          "status_helper_version",
          { ver: payloadVersion },
          `Helper v${payloadVersion}`,
        )
      : null,
  ]
    .filter(Boolean)
    .join("\n");

  return (
    // A quiet line on the atmosphere under the shell's canvas, lined up with
    // the canvas's inner edge.
    <div className="shell-statusbar hidden min-h-9 flex-wrap items-center gap-x-3 gap-y-1 whitespace-nowrap px-4 pb-[calc(var(--safe-bottom)_+_0.35rem)] pt-1.5 ps-[calc(var(--safe-left)_+_2rem)] pe-[calc(var(--safe-right)_+_2rem)] text-[0.6875rem] text-[var(--color-muted)] md:flex">
      {/* Group 1 — the Engine (our local backend). */}
      <div
        className="flex items-center gap-2"
        title={
          (engineError && shellErrorText(engineError, tr)) ??
          tr(
            "status_engine_tooltip",
            { url: getEngineUrl() },
            `ps5upload-engine — the app's backend (${getEngineUrl()})`,
          )
        }
      >
        {dot(engineLight)}
        <span>{tr("status_engine", undefined, "Engine")}</span>
        {dotText(engineLight)}
      </div>

      <span className="h-3 w-px bg-[var(--color-border)]" aria-hidden />

      {/* Group 2 — the PS5 console (helper + firmware). */}
      <div className="flex items-center gap-2" title={ps5Tooltip}>
        {dot(ps5Light)}
        <span className={ps5Connected ? undefined : "opacity-70"}>
          {ps5Name}
        </span>
        {ps5Light === "rest" || ps5Light === "unreachable" ? (
          <span data-testid="status-ps5-power">{consoleLightLabel(ps5Light, tr)}</span>
        ) : (
          dotText(ps5Light)
        )}
        {ps5Connected && ps5Firmware && (
          <span className="rounded-full border border-[var(--color-border-strong)] px-2 font-mono text-[0.625rem]">
            {tr("status_fw", { ver: ps5Firmware }, `FW ${ps5Firmware}`)}
          </span>
        )}
      </div>

      {otherConsoles.length > 0 && (
        <div className="flex items-center gap-1.5">
          {otherConsoles.map((p) => {
            const rt =
              runtimeByHost[hostOf(p.host) || "_"] ?? EMPTY_HOST_RUNTIME;
            const light = consoleLight(rt);
            const statusLabel =
              light === "up"
                ? tr("status_console_up", undefined, "connected")
                : light === "down"
                  ? tr("status_console_down", undefined, "not connected")
                  : consoleLightLabel(light, tr);
            return (
              <span
                key={p.id}
                title={`${p.name} — ${statusLabel}`}
                className="inline-flex items-center gap-1 rounded-full px-1 py-0.5 ring-1 ring-inset"
                style={{
                  // Accent ring identifies WHICH console; the inner dot
                  // carries its up/down state. Matches the tab stripe color.
                  ["--tw-ring-color" as string]:
                    profileAccentForHost(p.host, profiles) ?? "transparent",
                }}
              >
                {dot(light)}
              </span>
            );
          })}
        </div>
      )}

      {/* Right cluster — live activity + power/capture controls. */}
      <div className="ms-auto flex items-center gap-3">
        <ActivityStatusSlot />
        <OtherDevicesChip />
        <KeepAwakeIndicator />
        {/* Saving a capture is native; the web build's Bug Report attaches files instead. */}
        {isTauriEnv() && <CaptureButton />}
        {isTauriEnv() && <ViewCapturesButton />}
      </div>
    </div>
  );
}

/** The other devices using the selected console right now (8.0.0): a count, their names and
 *  what they are doing in the tooltip; a click opens Devices. Nothing when there are none or the
 *  helper predates the device list. */
function OtherDevicesChip() {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const state = useMeshStore((s) => meshFor(s, host).state);
  const others = othersOnline(state);
  if (!others.length) return null;
  const lines = others.map((d) => {
    const work = d.activity
      .filter((a) => a.kind !== "browsers")
      .map((a) => `${a.kind}${a.title ? ` ${a.title}` : ""}${typeof a.pct === "number" ? ` ${a.pct}%` : ""}`);
    return work.length ? `${deviceName(d)}: ${work.join(", ")}` : deviceName(d);
  });
  const busy = others.some((d) => d.activity.some((a) => a.kind !== "browsers"));
  const label = tr("mesh_chip", { count: others.length }, `${others.length} other device(s)`);
  return (
    <button
      type="button"
      onClick={() => navigate("/connection?tab=devices")}
      title={[label, ...lines].join("\n")}
      aria-label={label}
      className={`flex items-center gap-1 rounded-full px-1.5 py-0.5 hover:bg-[var(--color-surface)] ${busy ? "text-[var(--color-accent-bright)]" : ""}`}
    >
      <MonitorSmartphone size={12} aria-hidden />
      <span>{others.length}</span>
    </button>
  );
}

/**
 * "Keeping your PS5 awake" indicator. Renders only when the user picked
 * the "always" keep-awake policy AND at least one console's helper is
 * actually answering (the policy can't tick a console that's down).
 * Making the active policy visible matters: an invisible always-on
 * power behavior would feel like the PS5 "mysteriously stopped
 * sleeping" weeks after the user forgot the setting.
 */
function KeepAwakeIndicator() {
  const tr = useTr();
  const mode = useUploadSettingsStore((s) => s.keepPs5AwakeMode);
  const anyUp = useConnectionStore((s) => {
    for (const h in s.runtimeByHost) {
      if (s.runtimeByHost[h].payloadStatus === "up") return true;
    }
    return false;
  });
  if (mode !== "always" || !anyUp) return null;
  return (
    <span
      className="flex items-center gap-1 text-[var(--color-warn)]"
      title={tr(
        "status_keep_awake_tooltip_v2",
        "Keep-awake is set to “Always while connected” — connected consoles won't auto-enter rest mode while the app is open. Change in Settings → Console.",
      )}
    >
      <Zap size={11} aria-hidden />
      <span className="hidden sm:inline">
        {tr("status_keep_awake", "keeping awake")}
      </span>
    </span>
  );
}

/** Opens the viewer on the app's captures: look at, page through and delete them. */
function ViewCapturesButton() {
  const tr = useTr();
  const show = useCaptureViewer((s) => s.show);
  const label = tr("cap_view_all", undefined, "View captures");
  return (
    <button
      type="button"
      onClick={() => show()}
      title={label}
      aria-label={label}
      className="flex items-center rounded-full px-1.5 py-0.5 hover:bg-[var(--color-surface)]"
    >
      <Images size={13} />
    </button>
  );
}

/**
 * Global screenshot capture. Lives in the always-visible footer so a user can
 * grab WHATEVER screen shows the problem (not just the Bug Report form), with
 * the capture landing in the gallery the Bug Report page selects from.
 */
function CaptureButton() {
  const tr = useTr();
  const [state, setState] = useState<"idle" | "busy" | "done" | "fail">("idle");

  const capture = async () => {
    if (state === "busy") return;
    setState("busy");
    try {
      // Let the button's own spinner paint before the (synchronous-ish)
      // DOM serialization briefly blocks the main thread.
      await new Promise((r) => setTimeout(r, 16));
      const shot = await captureAppScreenshot();
      setState("done");
      useToastStore.getState().push({
        message: tr("cap_saved_toast", undefined, "Screenshot saved"),
        tone: "success",
        action: {
          label: tr("cap_view", undefined, "View"),
          onClick: () => useCaptureViewer.getState().show(shot.name),
        },
      });
    } catch {
      setState("fail");
    }
    setTimeout(() => setState("idle"), 1800);
  };

  const icon =
    state === "busy" ? (
      <Spinner size={14} />
    ) : state === "done" ? (
      <Check size={13} className="text-[var(--color-good)]" />
    ) : state === "fail" ? (
      <AlertTriangle size={13} className="text-[var(--color-bad)]" />
    ) : (
      <Camera size={13} />
    );

  return (
    <button
      type="button"
      onClick={capture}
      disabled={state === "busy"}
      title={tr(
        "status_capture_tooltip",
        undefined,
        "Capture a screenshot of this screen for a bug report",
      )}
      aria-label={tr("status_capture", undefined, "Capture screenshot")}
      className="flex items-center gap-1 rounded-full px-2 py-0.5 hover:bg-[var(--color-surface)]"
    >
      {icon}
      <span className="hidden sm:inline">
        {state === "done"
          ? tr("status_capture_done", undefined, "Saved")
          : state === "fail"
            ? tr("status_capture_fail", undefined, "Failed")
            : tr("status_capture", undefined, "Capture")}
      </span>
    </button>
  );
}
