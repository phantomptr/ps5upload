import { useEffect, useState } from "react";
import { create } from "zustand";
import { WifiOff } from "lucide-react";

import { Button } from "../components";
import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";
import { hostOf } from "../lib/addr";
import { fetchLinkHistory, hintToShow, type LinkHint, type LinkHistory } from "../lib/linkHistory";
import { useDocumentVisible } from "../lib/visibility";

/** The episode each console's hint was dismissed for (its first drop), this app session. A new
 *  episode shows the hint again. */
const useDismissedStore = create<{ since: Record<string, number> }>(() => ({ since: {} }));

/** How often the engine's record is read while the window shows. The engine answers from memory;
 *  nothing is sent to the console. */
const REFRESH_MS = 30_000;

export interface LinkDropBannerViewProps {
  hint: LinkHint | null;
  /** The console's own link, when known: the advice for a cable differs. */
  link: LinkHistory["link"];
  onDismiss: () => void;
}

export function LinkDropBannerView({ hint, link, onDismiss }: LinkDropBannerViewProps) {
  const tr = useTr();
  if (!hint) return null;
  const usb = hint.kind === "usb_wifi";
  const title = usb
    ? tr(
        "link_drops_usb_title",
        undefined,
        "The connection to the PS5 keeps dropping since a USB drive was connected.",
      )
    : tr("link_drops_title", undefined, "The connection to the PS5 keeps dropping.");
  const body = usb
    ? tr(
        "link_drops_usb_body",
        undefined,
        "USB 3 drives can disturb 2.4 GHz Wi-Fi: try a cable or 5 GHz Wi-Fi, or move the drive away from the console (a short USB extension cable helps).",
      )
    : link?.kind === "wired"
      ? tr("link_drops_body_wired", undefined, "Check the network cable and the router or switch it goes to.")
      : tr("link_drops_body", undefined, "Try a network cable or 5 GHz Wi-Fi.");
  return (
    <div
      role="status"
      className="mx-3 mt-3 rounded-[var(--radius-card)] border border-[color-mix(in_srgb,var(--color-warn)_35%,transparent)] bg-[var(--color-warn-soft)] px-4 py-2.5 backdrop-blur-xl md:mx-5 text-[var(--color-text)]"
    >
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:gap-3">
        <div className="flex min-w-0 flex-1 items-start gap-3">
          <WifiOff size={18} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
          <p className="text-sm">
            <span className="font-medium">{title}</span> {body}
          </p>
        </div>
        <div className="flex shrink-0 gap-2">
          <Button size="sm" variant="ghost" onClick={onDismiss}>
            {tr("link_drops_dismiss", undefined, "Dismiss")}
          </Button>
        </div>
      </div>
    </div>
  );
}

/** The active console's "keeps dropping" hint, from the engine's record (engine link_drops.rs).
 *  Shown only when the engine says the evidence fits; dismissed per episode. */
export default function LinkDropBanner() {
  const host = useConnectionStore((s) => s.host);
  const key = hostOf(host) || "";
  const status = useConnectionStore((s) => s.runtimeByHost[key || "_"]?.payloadStatus);
  const power = useConnectionStore((s) => s.runtimeByHost[key || "_"]?.power);
  const dismissed = useDismissedStore((s) => (key ? s.since[key] : undefined));
  const visible = useDocumentVisible();
  const [history, setHistory] = useState<{ key: string; h: LinkHistory } | null>(null);

  useEffect(() => {
    if (!visible || !key) return;
    let cancelled = false;
    const read = () => {
      fetchLinkHistory(key)
        .then((h) => {
          if (!cancelled) setHistory({ key, h });
        })
        // An engine that is down or older has no record: show nothing.
        .catch(() => {
          if (!cancelled) setHistory(null);
        });
    };
    read();
    const id = window.setInterval(read, REFRESH_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
    // `status` re-reads at once when the console goes down or comes back.
  }, [key, visible, status]);

  const current = history && history.key === key ? history.h : null;
  const hint = hintToShow(current, { dismissedSince: dismissed, resting: power === "rest" });
  return (
    <LinkDropBannerView
      hint={hint}
      link={current?.link ?? null}
      onDismiss={() => {
        if (!key || !hint) return;
        useDismissedStore.setState((s) => ({ since: { ...s.since, [key]: hint.since_ms } }));
      }}
    />
  );
}
