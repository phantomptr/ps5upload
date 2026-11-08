import { useNavigate } from "react-router";
import { Gamepad2, Moon, PlugZap, Power, ServerCrash, WifiOff } from "lucide-react";

import { useConnectionStore } from "../state/connection";
import { useHelperStopped } from "../state/helperStopped";
import { SendHelperButton } from "./SendHelperButton";
import { useTr } from "../state/lang";
import { useConsoleSettingForHost } from "../state/consoleSettings";
import { isTauriEnv } from "../lib/tauriEnv";
import { useConsoleWake } from "../lib/useConsoleWake";
import { REST_MODE_NETWORK_SETTING_PATH } from "../lib/consolePower";
import { Button } from "./Button";
import { EmptyState } from "./EmptyState";
import { ShapesLoader } from "./ShapesLoader";
import { Spinner } from "./Spinner";

/**
 * The one shared answer to "can this screen actually talk to a PS5?".
 *
 * Before v3 every console-backed screen handled (or silently failed to
 * handle) connectivity on its own: Hardware rendered four empty cards,
 * Shell accepted commands into the void, InstalledApps/FileSystem showed
 * blank space with no host, and the screens that did show something used
 * five different phrasings for the same situation. Each was a small
 * hand-rolled state machine — and each was a place for bugs to live.
 *
 * ConnectionGate centralizes the ladder, in priority order:
 *
 *   1. engine down   — the local sidecar is dead; nothing can work.
 *   2. no console    — fresh install / cleared roster; point to setup.
 *   3. helper down   — console known but the on-PS5 helper isn't
 *                      running (require="payload" only). Told apart:
 *                      stopped by the user, in rest mode (calm, with
 *                      Wake when this console's profile can), not
 *                      answering at all, or just no helper.
 *   4. probing       — status unknown, first check in flight.
 *   5. ready         — render children.
 *
 * Every non-ready state names the actual problem (no more "connect to
 * your PS5 first" covering three different causes) and carries a CTA
 * out of the dead-end.
 */
export function ConnectionGate({
  require = "payload",
  children,
}: {
  /** "host" = screen only needs an address configured (it probes on its
   *  own); "payload" = screen needs the on-PS5 helper answering. */
  require?: "host" | "payload";
  children: React.ReactNode;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const engineStatus = useConnectionStore((s) => s.engineStatus);
  const helperStopped = useHelperStopped(host ?? "");
  const power = useConnectionStore((s) => s.power);

  if (engineStatus === "down") {
    // The cause and the Restart button are in the notice every screen shows at the top
    // (EngineDownBanner); here only what it means for this screen.
    return (
      <EmptyState
        fill
        icon={ServerCrash}
        title={tr("gate_engine_down_title", "Transfer engine isn't running")}
        message={tr(
          "gate_engine_down_body_v2",
          "This screen needs the app's engine, which is not answering. The notice at the top of the window says why and can restart it.",
        )}
      />
    );
  }

  if (!host?.trim()) {
    return (
      <EmptyState
        fill
        icon={Gamepad2}
        title={tr("gate_no_console_title", "No PS5 connected yet")}
        message={tr(
          "gate_no_console_body",
          "Add your console's IP address on the Connection screen — after that, every screen here lights up.",
        )}
        action={
          <Button variant="primary" onClick={() => navigate("/connection")}>
            {tr("gate_setup_cta", "Set up connection")}
          </Button>
        }
      />
    );
  }

  if (require === "payload" && payloadStatus !== "up") {
    if (payloadStatus === "unknown") {
      // First probe still in flight — show the signature loader instead
      // of flashing a scary "not reachable" state for half a second.
      return (
        <div className="flex min-h-[60vh] items-center justify-center">
          <ShapesLoader
            size={20}
            label={tr("gate_checking", "Checking your PS5…")}
          />
        </div>
      );
    }
    if (helperStopped) {
      // Turned off on purpose (Connection → Stop helper, #471): not a fault.
      return (
        <EmptyState
          fill
          icon={PlugZap}
          title={tr("gate_helper_stopped_title", "You stopped the helper")}
          message={tr(
            "gate_helper_stopped_body_v2",
            "This screen needs the helper. Send it again when you need it.",
          )}
          action={
            <div className="flex flex-wrap items-start justify-center gap-2">
              <SendHelperButton />
              <Button variant="secondary" onClick={() => navigate("/connection")}>
                {tr("gate_send_helper_cta", "Open Connection")}
              </Button>
            </div>
          }
        />
      );
    }
    if (power === "rest") return <RestModeGate host={host} />;
    if (power === "unreachable") return <PowerNotice power="unreachable" />;
    return (
      <EmptyState
        fill
        icon={PlugZap}
        title={tr("gate_payload_down_title", "Helper isn't running on the PS5")}
        message={tr(
          "gate_payload_down_body_v2",
          "Your console is configured but the ps5upload helper isn't answering — it drops on every reboot or rest mode. Send it again to carry on.",
        )}
        action={
          // The helper is the one thing missing: send it from here.
          <div className="flex flex-wrap items-start justify-center gap-2">
            <SendHelperButton />
            <Button variant="secondary" onClick={() => navigate("/connection")}>
              {tr("gate_send_helper_cta", "Open Connection")}
            </Button>
          </div>
        }
      />
    );
  }

  return <>{children}</>;
}

/** The rest-mode notice wired to this console's wake credentials and the reconnect setting. */
function RestModeGate({ host }: { host: string }) {
  const wake = useConsoleWake(host);
  // Desktop with "send the helper again after a wake" on: the helper comes back unasked.
  const autoRedeploy = useConsoleSettingForHost(host, "auto_redeploy_on_wake");
  return (
    <PowerNotice
      power="rest"
      helperReturns={isTauriEnv() && autoRedeploy}
      canWake={wake.canWake}
      canSignIn={wake.canSignIn}
      waking={wake.busy}
      wakeError={wake.error}
      onWake={() => void wake.wake()}
    />
  );
}

/**
 * What a console screen says when the console is asleep or not answering — calm, because
 * neither is a fault of the app or the helper. Pure (props only) so each state can be
 * rendered in a test.
 */
export function PowerNotice({
  power,
  helperReturns = false,
  canWake = false,
  canSignIn = false,
  waking = false,
  wakeError = null,
  onWake,
}: {
  power: "rest" | "unreachable";
  /** The helper is sent again by itself once the console is awake. */
  helperReturns?: boolean;
  /** This console's profile holds a wake credential or session keys. */
  canWake?: boolean;
  canSignIn?: boolean;
  waking?: boolean;
  wakeError?: string | null;
  onWake?: () => void;
}) {
  const tr = useTr();
  if (power === "unreachable") {
    return (
      <EmptyState
        fill
        icon={WifiOff}
        title={tr("gate_not_answering_title", "Your PS5 isn't answering")}
        message={tr(
          "gate_not_answering_body",
          {
            // The console's own menu path, as the wake setup already translates it.
            path: tr("power_wake_req_restmode_path", undefined, REST_MODE_NETWORK_SETTING_PATH),
          },
          "It is off, on another network, or in rest mode with its network features off. To reach it while it rests, turn them on in {path}.",
        )}
      />
    );
  }
  const body = canWake
    ? helperReturns
      ? tr(
          "gate_rest_body_auto",
          "Wake it with the controller or Wake below. The helper comes back by itself once it is awake.",
        )
      : tr(
          "gate_rest_body_manual",
          "Wake it with the controller or Wake below, then send the helper.",
        )
    : helperReturns
      ? tr(
          "gate_rest_body_auto_nowake",
          "Wake it with the controller. The helper comes back by itself once it is awake.",
        )
      : tr(
          "gate_rest_body_manual_nowake",
          "Wake it with the controller, then send the helper.",
        );
  return (
    <EmptyState
      fill
      icon={Moon}
      title={tr("gate_rest_title", "Your PS5 is in rest mode")}
      body={
        <>
          <span>{body}</span>
          {wakeError && (
            <span className="mt-2 block text-xs text-[var(--color-bad)]">{wakeError}</span>
          )}
        </>
      }
      action={
        canWake ? (
          <Button
            variant="primary"
            onClick={onWake}
            disabled={waking}
            leftIcon={waking ? <Spinner size={12} tone="inherit" /> : <Power size={12} />}
          >
            {waking && canSignIn
              ? tr("power_action_wake_signin_busy", undefined, "Waking & signing in…")
              : canSignIn
                ? tr("power_action_wake_signin", undefined, "Wake & sign in")
                : tr("power_action_wake", undefined, "Wake")}
          </Button>
        ) : undefined
      }
    />
  );
}
