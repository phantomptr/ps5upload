import { useEffect, useMemo } from "react";
import { useNavigate, useSearchParams } from "react-router";

import { PadApp } from "../../pad/PadApp";
import { getEngineUrl } from "../../state/engine";
import { focusForControllers, holdPageStill, lockLandscape, setWindowFullscreen, unlockOrientation } from "./immersive";

/** This app's engine relays to another computer's controller socket (pad_link.rs relay): an
 *  app page may not open a plain ws:// to another machine, its own loopback it may. */
export function relayUrl(engineUrl: string, target: string, link?: { key: string; watch: boolean }): string {
  const base = engineUrl.replace(/^http/, "ws").replace(/\/$/, "");
  const extra = link ? `&t=${encodeURIComponent(link.key)}${link.watch ? "&w=1" : ""}` : "";
  return `${base}/api/ps5/rp/pad-relay?url=${encodeURIComponent(target)}${extra}`;
}

/** This phone as the controller of a Remote Play session another device runs
 *  (/play/pad?url=<its pad socket>&name=<its name>[&t=<its link's key>][&w=1]): full screen and
 *  sideways. With a link's key (a QR code or link opened in the app) it is in without asking;
 *  with w=1 it watches. */
export default function PadRemote() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const target = params.get("url") ?? "";
  const computer = params.get("name") ?? "";
  const key = params.get("t");
  const watch = params.get("w") === "1";
  const url = useMemo(
    () => relayUrl(getEngineUrl(), target, key ? { key, watch } : undefined),
    [target, key, watch],
  );

  useEffect(() => {
    holdPageStill(true);
    void setWindowFullscreen(true);
    lockLandscape();
    focusForControllers();
    return () => {
      unlockOrientation();
      void setWindowFullscreen(false);
      holdPageStill(false);
    };
  }, []);

  return <PadApp url={url} computer={computer} watch={watch} onLeave={() => navigate("/play")} />;
}
