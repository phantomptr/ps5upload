import { lazy, Suspense, useEffect, useRef } from "react";
import { useLocation } from "react-router";

import { isLive } from "../lib/goLive";
import { closeLeftoverPipPopups } from "../lib/pipPopups";
import { justFailed, problemText, runSite } from "../screens/Play/goLiveText";
import { RpPasscodeBox } from "../screens/Play/RpPasscodeBox";
import { streamErrorMessage } from "../screens/Play/streamErrors";
import { useGoLiveStore } from "../state/goLive";
import { useTr } from "../state/lang";
import { pushNotification } from "../state/notifications";
import { useRemotePlayStore } from "../state/remotePlay";

const RemotePlayAsks = lazy(() => import("./RemotePlayAsks"));

/** How often a Go live run is looked at while the Play screen (which polls it) isn't open. */
const LIVE_POLL_MS = 3000;

/**
 * What a running stream or Go live run needs someone to see, wherever the app is: the Play screen's
 * stage has all of this only while it is what's on show. The passcode the PS5 asks for, friends and
 * phones asking in, why a stream or a broadcast stopped.
 */
export function RemotePlayHost() {
  const tr = useTr();
  const { pathname } = useLocation();
  const onPlay = pathname === "/play";
  const stream = useRemotePlayStore((s) => s.stream);
  const host = useRemotePlayStore((s) => s.target?.host ?? null);
  const pip = useRemotePlayStore((s) => !!s.pip);
  const passcode = useRemotePlayStore((s) => s.passcode);
  const error = useRemotePlayStore((s) => s.error);
  const away = !onPlay || pip;

  // A page reload in the app leaves its always-on-top window with nothing to show.
  useEffect(() => {
    void closeLeftoverPipPopups();
  }, []);

  const passcodeSeq = passcode?.seq ?? null;
  useEffect(() => {
    if (passcodeSeq === null || !away) return;
    pushNotification("warning", tr("rp_passcode_title", undefined, "Enter this PS5 user's passcode"), { link: "/play" });
    // Once per ask.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [passcodeSeq]);

  useEffect(() => {
    if (!error || onPlay) return;
    pushNotification("error", streamErrorMessage(tr, error), { link: "/play" });
    // Once per error.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [error]);

  return (
    <>
      <GoLiveWatch polling={!onPlay} />
      {/* On Play the stage has the box, under picture in picture too. */}
      {stream && !onPlay && <RpPasscodeBox placement="screen" />}
      {stream && host && away && (
        <Suspense fallback={null}>
          <RemotePlayAsks host={host} floating={!onPlay} />
        </Suspense>
      )}
    </>
  );
}

/** A broadcast that stopped is said wherever the app is, and full screen: the LIVE chip just going
 *  away tells a streamer nothing. */
function GoLiveWatch({ polling }: { polling: boolean }) {
  const tr = useTr();
  const run = useGoLiveStore((s) => s.status?.live ?? null);
  const refresh = useGoLiveStore((s) => s.refresh);
  const live = isLive(run);
  const wasLive = useRef(false);
  useEffect(() => {
    const failed = justFailed(wasLive.current, run);
    if (failed && run) {
      pushNotification("error", tr("golive_stopped", undefined, "The stream stopped"), {
        body: problemText(tr, failed.code, failed.message, runSite(tr, run)),
        link: "/play",
      });
    }
    wasLive.current = live;
  }, [run, live, tr]);
  useEffect(() => {
    if (!live || !polling) return;
    const t = window.setInterval(() => void refresh(), LIVE_POLL_MS);
    return () => window.clearInterval(t);
  }, [live, polling, refresh]);
  return null;
}
