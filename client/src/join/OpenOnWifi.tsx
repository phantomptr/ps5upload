import { useEffect, useState } from "react";

import { useTr } from "../state/lang";
import { HUB_PROBE_MS, probeHub } from "./lanHint";

/** True once the hub at `hubUrl` did not answer within HUB_PROBE_MS (asked once, only when the
 *  link names a Wi-Fi page: `lan`). */
export function useHubDown(hubUrl: string | null, lan: string | null): boolean {
  const [down, setDown] = useState(false);
  useEffect(() => {
    if (!hubUrl || !lan) return;
    let alive = true;
    void probeHub(hubUrl, HUB_PROBE_MS).then((ok) => {
      if (alive && !ok) setDown(true);
    });
    return () => {
      alive = false;
    };
  }, [hubUrl, lan]);
  return down;
}

/** "Open on this Wi-Fi": the host's own page on their home network, for a friend whose browser
 *  can't reach the invite service (no internet here, or it is down) but who is on that Wi-Fi. A
 *  link the friend taps (a secure page can't fetch the LAN page, it can only go there). The
 *  classes are ones the guest pages already use (the page's size budget). */
export function OpenOnWifi({ href }: { href: string }) {
  const tr = useTr();
  return (
    <div className="flex flex-col items-center gap-3 text-center" data-testid="open-on-wifi">
      <p className="max-w-sm text-sm text-white/60">
        {tr("join_open_lan_body", undefined, "Can't reach the internet? If you're on the same Wi-Fi as your friend, open the game there.")}
      </p>
      <a href={href} rel="noreferrer" className="rounded-full bg-white px-4 py-2 text-sm font-medium text-black" data-testid="open-on-wifi-link">
        {tr("join_open_lan", undefined, "Open on this Wi-Fi")}
      </a>
    </div>
  );
}
