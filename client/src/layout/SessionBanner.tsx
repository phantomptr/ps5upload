import { KeyRound } from "lucide-react";

import { Button } from "../components";
import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";
import { usePairingStore } from "../state/pairing";
import { hostOf } from "../lib/addr";
import { type SessionState } from "../lib/consoleSession";

export interface SessionBannerViewProps {
  session: SessionState | null;
  onPair: () => void;
}

/** What the one status probe asks of the person: pair. Nothing for connected or down
 *  (down has the Send helper flow on the Connection screen). */
export function SessionBannerView({ session, onPair }: SessionBannerViewProps) {
  const tr = useTr();
  if (session === "needs_pairing") {
    return (
      <div className="mx-3 mt-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-warn)_35%,transparent)] bg-[var(--color-warn-soft)] px-4 py-2.5 backdrop-blur-xl md:mx-5 text-[var(--color-text)]">
        <div className="mx-auto flex max-w-6xl flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
          <div className="flex min-w-0 flex-1 items-start gap-3">
            <KeyRound size={18} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
            <p className="text-sm font-medium">
              {tr(
                "session_needs_pairing",
                undefined,
                "This PS5 has not accepted this app yet. Pair them to continue.",
              )}
            </p>
          </div>
          <Button size="sm" variant="primary" onClick={onPair}>
            {tr("session_pair_button", undefined, "Pair…")}
          </Button>
        </div>
      </div>
    );
  }
  return null;
}

/** The active console's banner. */
export default function SessionBanner() {
  const host = useConnectionStore((s) => s.host);
  const rt = useConnectionStore(
    (s) => s.runtimeByHost[hostOf(host) || "_"],
  );
  const openPairing = usePairingStore((s) => s.openFor);
  if (!host.trim() || !rt) return null;
  return <SessionBannerView session={rt.session} onPair={() => void openPairing(host)} />;
}
