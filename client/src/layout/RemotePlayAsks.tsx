import { useEffect, useRef } from "react";

import { askText, PadAsks, usePadLink } from "../screens/Play/PhonePads";
import { JoinAsks } from "../screens/Play/PlayTogether";
import { useJoinHost } from "../screens/Play/playTogetherState";
import { useTr } from "../state/lang";
import { pushNotification } from "../state/notifications";

/**
 * Friends and phones asking in while the Play screen isn't what's on show: they would otherwise be
 * refused after 90 s with the host none the wiser. A notification each, and the Allow box over the
 * app (`floating`; in picture in picture the small player has its own, pip.tsx).
 */
export default function RemotePlayAsks({ host, floating }: { host: string; floating: boolean }) {
  const tr = useTr();
  const [pad] = usePadLink(true, host);
  const [join] = useJoinHost(true, host);
  const told = useRef(new Set<string>());
  useEffect(() => {
    const say = (id: string, title: string) => {
      if (told.current.has(id)) return;
      told.current.add(id);
      pushNotification("info", title, { link: "/play" });
    };
    for (const a of join.asks) {
      const name = a.name || tr("pt_friend_fallback", undefined, "Friend");
      say(`j${a.id}`, tr("pt_asks", { name }, `${name} wants to play with you`));
    }
    for (const p of pad.pending) {
      // An internet phone that would play is said as what it is: control of the PS5 as the host.
      say(`p${p.id}`, askText(tr, p));
    }
  }, [join.asks, pad.pending, tr]);
  if (!floating) return null;
  return (
    <div className="pointer-events-none fixed inset-x-0 top-0 z-50 h-0" data-testid="rp-asks-host">
      <PadAsks pending={pad.pending} />
      <JoinAsks addr={host} asks={join.asks} seatNext={join.seatNext} nextProfile={join.nextProfile?.name ?? null} />
    </div>
  );
}
