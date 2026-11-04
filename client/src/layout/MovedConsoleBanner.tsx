import { useEffect } from "react";
import { create } from "zustand";
import { ArrowRightLeft } from "lucide-react";

import { Button } from "../components";
import { sameConsole } from "../api/ava1";
import { discoverPs5 } from "../api/ps5";
import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";
import { selectConsoleByAddress, useRosterStore } from "../state/roster";
import { hostOf } from "../lib/addr";
import { findMovedConsole, shouldLook } from "../lib/movedConsole";
import { isTauriEnv } from "../lib/tauriEnv";

/** The console the app points at stopped answering, and the same console (proved by its
 *  key) answers at `to`. Cleared by Switch, Dismiss, or the app pointing elsewhere. */
interface MovedConsoleState {
  offer: { from: string; to: string } | null;
  /** When each address was last looked for, so a down console is scanned for at most
   *  every few minutes. */
  lastLookAt: Record<string, number>;
  looking: boolean;
}

const useMovedConsoleStore = create<MovedConsoleState>(() => ({
  offer: null,
  lastLookAt: {},
  looking: false,
}));

export interface MovedConsoleBannerViewProps {
  offer: { from: string; to: string } | null;
  activeHost: string;
  onSwitch: (to: string) => void;
  onDismiss: () => void;
}

export function MovedConsoleBannerView({
  offer,
  activeHost,
  onSwitch,
  onDismiss,
}: MovedConsoleBannerViewProps) {
  const tr = useTr();
  if (!offer || hostOf(offer.from) !== hostOf(activeHost)) return null;
  return (
    <div className="mx-3 mt-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-accent)_35%,transparent)] bg-[var(--color-accent-soft)] px-4 py-2.5 backdrop-blur-xl md:mx-5 text-[var(--color-text)]">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <ArrowRightLeft size={18} className="mt-0.5 shrink-0 text-[var(--color-accent)]" />
          <p className="text-sm">
            <span className="font-medium">
              {tr(
                "moved_console_title",
                { to: offer.to },
                `Your PS5 now answers at ${offer.to}.`,
              )}
            </span>{" "}
            {tr(
              "moved_console_body",
              { from: offer.from },
              `It is the same console, but its address on your network changed (it was ${offer.from}).`,
            )}
          </p>
        </div>
        <div className="flex shrink-0 gap-2">
          <Button size="sm" variant="primary" onClick={() => onSwitch(offer.to)}>
            {tr("moved_console_switch", { to: offer.to }, `Switch to ${offer.to}`)}
          </Button>
          <Button size="sm" variant="ghost" onClick={onDismiss}>
            {tr("moved_console_dismiss", undefined, "Dismiss")}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** The active console's banner. */
export default function MovedConsoleBanner() {
  const host = useConnectionStore((s) => s.host);
  const offer = useMovedConsoleStore((s) => s.offer);
  return (
    <MovedConsoleBannerView
      offer={offer}
      activeHost={host}
      onSwitch={(to) => {
        useMovedConsoleStore.setState({ offer: null });
        selectConsoleByAddress(to);
      }}
      onDismiss={() => useMovedConsoleStore.setState({ offer: null })}
    />
  );
}

/** How often a down console is re-checked against `shouldLook`'s back-off. */
const TICK_MS = 60_000;

/** While the active console is down, look for it at another address (desktop and Android:
 *  the scan is mDNS, which a browser cannot do). Mounted once, in AppShell. */
export function useMovedConsoleWatch() {
  const host = useConnectionStore((s) => s.host);
  const status = useConnectionStore(
    (s) => s.runtimeByHost[hostOf(s.host) || "_"]?.payloadStatus ?? "unknown",
  );
  const power = useConnectionStore((s) => s.runtimeByHost[hostOf(s.host) || "_"]?.power);
  useEffect(() => {
    if (!isTauriEnv() || !host.trim() || status !== "down" || power === "rest") return;
    const look = async () => {
      const st = useMovedConsoleStore.getState();
      const from = hostOf(host);
      if (st.looking || !shouldLook(status, st.lastLookAt[from], Date.now(), power)) return;
      useMovedConsoleStore.setState({
        looking: true,
        lastLookAt: { ...st.lastLookAt, [from]: Date.now() },
      });
      try {
        const roster = useRosterStore.getState().profiles.map((p) => p.host);
        const to = await findMovedConsole(host, roster, {
          discover: async () => (await discoverPs5(3)).candidates,
          sameConsole,
        });
        // Still pointing at the old address, and still unable to reach it.
        const now = useConnectionStore.getState();
        const stillDown = now.runtimeByHost[from]?.payloadStatus === "down";
        if (to && hostOf(now.host) === from && stillDown) {
          useMovedConsoleStore.setState({ offer: { from, to } });
        }
      } finally {
        useMovedConsoleStore.setState({ looking: false });
      }
    };
    void look();
    const id = window.setInterval(() => void look(), TICK_MS);
    return () => window.clearInterval(id);
  }, [host, status, power]);
}
