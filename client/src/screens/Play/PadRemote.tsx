import { useEffect, useMemo, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";

import { padDeviceId } from "../../lib/padClient";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { PadApp } from "../../pad/PadApp";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { getEngineUrl } from "../../state/engine";
import { focusForControllers, holdPageStill, lockLandscape, setWindowFullscreen, unlockOrientation } from "./immersive";

/** How long a second Back counts as meaning it. */
const BACK_AGAIN_MS = 3000;

/** This app's engine relays to another computer's controller socket (pad_link.rs relay): an
 *  app page may not open a plain ws:// to another machine, its own loopback it may. */
export function relayUrl(engineUrl: string, target: string, link?: { key: string; watch: boolean }, device?: string): string {
  const base = engineUrl.replace(/^http/, "ws").replace(/\/$/, "");
  const extra = link ? `&t=${encodeURIComponent(link.key)}${link.watch ? "&w=1" : ""}` : "";
  return `${base}/api/ps5/rp/pad-relay?url=${encodeURIComponent(target)}${extra}${device ? `&d=${device}` : ""}`;
}

/** This phone as the controller of a Remote Play session another device runs
 *  (/play/pad?url=<its pad socket>&name=<its name>[&t=<its link's key>][&w=1]): full screen and
 *  sideways. With a link's key (a QR code or link opened in the app) it is in without asking;
 *  with w=1 it watches. */
export default function PadRemote() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const tr = useTr();
  const key = params.get("t");
  const computer = params.get("name") ?? "";
  const watch = params.get("w") === "1";
  // A link opened in the app (a ps5upload:// link from a page or message) connects only once the
  // user says so: the link could come from anyone on the network.
  const [confirmed, setConfirmed] = useState(!key);
  if (!confirmed) {
    return (
      <div className="flex min-h-[60vh] items-center justify-center p-6" data-testid="pad-link-confirm">
        <div className="w-full max-w-sm space-y-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-5">
          <h2 className="text-base font-semibold">
            {tr("pad_link_confirm_title", { host: computer }, `Connect to ${computer}?`)}
          </h2>
          <p className="text-sm text-[var(--color-muted)]">
            {watch
              ? tr("pad_link_confirm_watch", { host: computer }, `This device watches the PS5 that ${computer} is playing.`)
              : tr("pad_link_confirm_play", { host: computer }, `This device becomes a controller for the PS5 that ${computer} is playing.`)}
          </p>
          <div className="flex gap-2">
            <Button variant="primary" onClick={() => setConfirmed(true)} data-testid="pad-link-connect">
              {tr("pad_link_connect", undefined, "Connect")}
            </Button>
            <Button variant="secondary" onClick={() => navigate("/play")}>
              {tr("join_cancel", undefined, "Cancel")}
            </Button>
          </div>
        </div>
      </div>
    );
  }
  return <PadRemoteLive />;
}

/** The controller itself, once connecting is agreed. */
function PadRemoteLive() {
  const tr = useTr();
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const target = params.get("url") ?? "";
  const computer = params.get("name") ?? "";
  const key = params.get("t");
  const watch = params.get("w") === "1";
  const url = useMemo(
    () => relayUrl(getEngineUrl(), target, key ? { key, watch } : undefined, padDeviceId({ get: safeGetItem, set: safeSetItem })),
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

  // System Back here would drop the controller at once, mid-game: the first press only warns.
  const [backWarned, setBackWarned] = useState(false);
  useEffect(() => {
    // Same state as the page's own entry: the router sees no navigation.
    window.history.pushState(window.history.state, "");
    let warnedUntil = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const onPop = () => {
      const now = Date.now();
      if (now < warnedUntil) {
        navigate("/play", { replace: true });
        return;
      }
      warnedUntil = now + BACK_AGAIN_MS;
      window.history.pushState(window.history.state, "");
      setBackWarned(true);
      clearTimeout(timer);
      timer = setTimeout(() => setBackWarned(false), BACK_AGAIN_MS);
    };
    window.addEventListener("popstate", onPop);
    return () => {
      window.removeEventListener("popstate", onPop);
      clearTimeout(timer);
    };
  }, [navigate]);

  return (
    <>
      <PadApp url={url} computer={computer} watch={watch} onLeave={() => navigate("/play", { replace: true })} />
      {backWarned && (
        <div
          role="status"
          className="pointer-events-none fixed left-1/2 top-[max(0.75rem,env(safe-area-inset-top))] z-50 -translate-x-1/2 rounded-full bg-black/70 px-3 py-1.5 text-xs text-white"
        >
          {tr("pad_back_again", undefined, "Press Back again to leave.")}
        </div>
      )}
    </>
  );
}
