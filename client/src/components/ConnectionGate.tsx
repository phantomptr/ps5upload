import { useNavigate } from "react-router";
import { Gamepad2, PlugZap, ServerCrash } from "lucide-react";

import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";
import { Button } from "./Button";
import { EmptyState } from "./EmptyState";
import { ShapesLoader } from "./ShapesLoader";

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
 *                      running (require="payload" only).
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

  if (engineStatus === "down") {
    return (
      <EmptyState
        fill
        icon={ServerCrash}
        title={tr("gate_engine_down_title", "Transfer engine isn't running")}
        message={tr(
          "gate_engine_down_body",
          "The local engine that talks to your PS5 has stopped. Restarting the app brings it back; the Logs screen shows why it exited.",
        )}
        action={
          <Button variant="secondary" onClick={() => navigate("/logs")}>
            {tr("gate_view_logs", "View logs")}
          </Button>
        }
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
    return (
      <EmptyState
        fill
        icon={PlugZap}
        title={tr("gate_payload_down_title", "Helper isn't running on the PS5")}
        message={tr(
          "gate_payload_down_body",
          "Your console is configured but the ps5upload helper isn't answering — it drops on every reboot or rest mode. Send it again from the Connection screen.",
        )}
        action={
          <Button variant="primary" onClick={() => navigate("/connection")}>
            {tr("gate_send_helper_cta", "Open Connection")}
          </Button>
        }
      />
    );
  }

  return <>{children}</>;
}
