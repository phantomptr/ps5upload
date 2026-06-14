import { useState } from "react";
import { Camera, Check, Loader2, AlertTriangle, Zap } from "lucide-react";

import { useConnectionStore, EMPTY_HOST_RUNTIME } from "../state/connection";
import { parsePS5Firmware } from "../lib/ps5Firmware";
import { useTr } from "../state/lang";
import { captureAppScreenshot } from "../lib/captureScreenshot";
import {
  useRosterStore,
  useActiveProfile,
  profileAccentForHost,
} from "../state/roster";
import { useActivityHistoryStore } from "../state/activityHistory";
import { useUploadSettingsStore } from "../state/uploadSettings";
import { getEngineUrl } from "../state/engine";
import { hostOf } from "../lib/addr";

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
  // Live "what's running" count — replaces the old hardcoded
  // "no active transfers" label that never updated.
  const runningCount = useActivityHistoryStore(
    (s) => s.entries.filter((e) => e.outcome === "running").length,
  );

  const dot = (status: "up" | "down" | "unknown") => {
    const color =
      status === "up"
        ? "bg-[var(--color-good)]"
        : status === "down"
          ? "bg-[var(--color-bad)]"
          : "bg-[var(--color-muted)]";
    return (
      <span
        className={`inline-block h-2 w-2 rounded-full ${color}`}
        aria-hidden
      />
    );
  };

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
  const ps5Tooltip = [
    ps5Connected
      ? tr("status_ps5_connected", undefined, "Connected")
      : tr("status_ps5_disconnected", undefined, "Not connected"),
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
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 whitespace-nowrap border-t border-[var(--color-border)] bg-[var(--color-surface-2)] px-4 pt-1.5 pb-[calc(env(safe-area-inset-bottom)_+_0.375rem)] pl-[calc(env(safe-area-inset-left)_+_1rem)] pr-[calc(env(safe-area-inset-right)_+_1rem)] text-xs text-[var(--color-muted)]">
      {/* Group 1 — the Engine (our local backend). */}
      <div
        className="flex items-center gap-2"
        title={
          engineError ??
          tr(
            "status_engine_tooltip",
            { url: getEngineUrl() },
            `ps5upload-engine — the app's backend (${getEngineUrl()})`,
          )
        }
      >
        {dot(engineStatus)}
        <span>{tr("status_engine", undefined, "Engine")}</span>
      </div>

      <span className="h-3 w-px bg-[var(--color-border)]" aria-hidden />

      {/* Group 2 — the PS5 console (helper + firmware). */}
      <div className="flex items-center gap-2" title={ps5Tooltip}>
        {dot(payloadStatus)}
        <span className={ps5Connected ? undefined : "opacity-70"}>
          {ps5Name}
        </span>
        {ps5Connected && ps5Firmware && (
          <span className="rounded bg-[var(--color-surface-3)] px-1 font-mono text-xs">
            {tr("status_fw", { ver: ps5Firmware }, `FW ${ps5Firmware}`)}
          </span>
        )}
      </div>

      {otherConsoles.length > 0 && (
        <div className="flex items-center gap-1.5">
          {otherConsoles.map((p) => {
            const rt =
              runtimeByHost[hostOf(p.host) || "_"] ?? EMPTY_HOST_RUNTIME;
            const statusLabel =
              rt.payloadStatus === "up"
                ? tr("status_console_up", undefined, "connected")
                : rt.payloadStatus === "down"
                  ? tr("status_console_down", undefined, "not connected")
                  : tr("status_console_unknown", undefined, "not probed yet");
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
                {dot(rt.payloadStatus)}
              </span>
            );
          })}
        </div>
      )}

      {/* Right cluster — live activity + power/capture controls. */}
      <div className="ms-auto flex items-center gap-3">
        <span>
          {runningCount > 0
            ? tr(
                "status_running_count",
                { count: runningCount },
                `${runningCount} running`,
              )
            : tr(
                "status_no_active_transfers",
                undefined,
                "No active transfers",
              )}
        </span>
        <KeepAwakeIndicator />
        <CaptureButton />
      </div>
    </div>
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
        "status_keep_awake_tooltip",
        "Keep-awake is set to “Always while connected” — connected consoles won't auto-enter rest mode while the app is open. Change in Settings → Upload.",
      )}
    >
      <Zap size={11} aria-hidden />
      <span className="hidden sm:inline">
        {tr("status_keep_awake", "keeping awake")}
      </span>
    </span>
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
      await captureAppScreenshot();
      setState("done");
    } catch {
      setState("fail");
    }
    setTimeout(() => setState("idle"), 1800);
  };

  const icon =
    state === "busy" ? (
      <Loader2 size={13} className="animate-spin" />
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
      className="flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-[var(--color-surface-3)]"
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
