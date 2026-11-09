import { ListChecks, LogIn, MonitorPlay, Users } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";

import { Button, PageHeader, SegmentedControl } from "../../components";
import { useTr } from "../../state/lang";
import "../../state/playSetup";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useConnectionStore } from "../../state/connection";
import { useRosterStore } from "../../state/roster";
import { useStreamLaunch } from "./game/streamLaunch";
import { usePlaySetup } from "./setup/usePlaySetup";
import { COMPACT_QUERY, useMedia } from "./immersive";
import { StreamPanel } from "./StreamPanel";
import { JoinAFriend } from "./JoinAFriend";
import { useJoinStore } from "../../state/joinSession";

/** Play my PS5 (Remote Play), Play together (host friends on it), Join a friend's game, or
 *  Receive what a friend shared (a Share link). */
export type PlayMode = "mine" | "together" | "friend";

/**
 * Play (was "Remote Play" before 10.0.0): a console's screen streamed into the app, played with a
 * controller or the keyboard (the chiaki-ng port, engine rp_stream). Getting a console ready is
 * Play › Set up (setup/); pairing Sony's own PS Remote Play app is in Accounts.
 */
export default function PlayScreen() {
  const tr = useTr();
  const navigate = useNavigate();
  const compact = useMedia(COMPACT_QUERY);
  const streaming = useRemotePlayStore(
    (s) => !!s.stream && s.phase !== null && s.phase !== "ended",
  );
  // Play Together (#641): Play my PS5 (Remote Play) or Join a friend's game, one at a time.
  const joined = useJoinStore((s) => !!s.session);
  // /play?launch=<TITLE>[&name=…] (search, links): start that game and stream it; a console not
  // set up for Play goes to its setup instead.
  const [params, setParams] = useSearchParams();
  const launch = params.get("launch")?.trim().toUpperCase() ?? "";
  const launchName = params.get("name")?.trim() ?? "";
  // /play?join=<a friend's Play Together link>: Join a friend, with that link.
  const joinParam = params.get("join")?.trim() || null;
  // /play?receive=<a friend's Share link> (before 12.3): Share has it now.
  const receiveParam = params.get("receive")?.trim() || null;
  useEffect(() => {
    if (receiveParam) navigate(`/share?receive=${encodeURIComponent(receiveParam)}`, { replace: true });
  }, [receiveParam, navigate]);
  const [mode, setMode] = useState<PlayMode>(() =>
    joined || joinParam ? "friend" : "mine",
  );
  // Play together is the same stream as Play my PS5 with its invite panel (StreamPanel stays
  // mounted between the two, so switching never touches the stream). The stream bar's button
  // and the menu switch here and back.
  const onTogether = useCallback((open: boolean) => setMode(open ? "together" : "mine"), []);
  const [pendingJoin, setPendingJoin] = useState<string | null>(joinParam);
  useEffect(() => {
    if (!joinParam) return;
    setPendingJoin(joinParam);
    setMode("friend");
    setParams({}, { replace: true });
  }, [joinParam, setParams]);
  const host = useConnectionStore((s) => s.host)?.trim() ?? "";
  const paired = useRosterStore((s) => {
    const p = s.profiles.find((x) => x.host === host);
    return !!(p?.wake_regist_key && p?.wake_rp_key);
  });
  // The header's Set up button shows how far along a console is until it's paired.
  const setup = usePlaySetup({ poll: !paired });
  const setupDone = setup.ready;
  useEffect(() => {
    if (!/^[A-Z]{4}\d{5}$/.test(launch)) return;
    setParams({}, { replace: true });
    if (!host || !paired) {
      navigate("/play/setup");
      return;
    }
    // The launch plays on the stream (StreamPanel), not on Join a friend.
    setMode((m) => (m === "friend" ? "mine" : m));
    useStreamLaunch.getState().request(host, { titleId: launch, titleName: launchName || launch });
  }, [launch, launchName, host, paired, navigate, setParams]);
  // On a phone the picture and its controller need the height: while playing, the header is
  // just the title. Only while the stream on screen is the one running: on Join a friend with
  // your own Play running (a link opened, then the switch declined), the switch stays to go back.
  const lean = compact && ((streaming && mode === "mine") || (joined && mode === "friend"));
  return (
    <div className="app-page space-y-4">
      <PageHeader
        icon={MonitorPlay}
        title={tr("play_title", undefined, "Play")}
        description={
          lean
            ? undefined
            : tr(
                "rp_play_description2",
                undefined,
                "Stream a PS5 into this app and play it with a controller, the keyboard or touch. It wakes a console in rest mode on its own, and keeps playing while you use other screens.",
              )
        }
        right={
          // Easy to find (10.0.1): a filled button with the progress while setup isn't done.
          <Button
            variant={setupDone ? "secondary" : "primary"}
            size="md"
            leftIcon={<ListChecks size={16} />}
            onClick={() => navigate("/play/setup")}
            data-testid="play-setup-button"
          >
            {setupDone
              ? tr("play_setup_short", undefined, "Set up")
              : tr("play_setup_button_progress", { done: setup.progress.done, total: setup.progress.total }, `Set up Play · ${setup.progress.done}/${setup.progress.total}`)}
          </Button>
        }
      />
      {!lean && (
        <SegmentedControl
          ariaLabel={tr("play_mode_label", undefined, "What to play")}
          value={mode}
          onChange={(v) => setMode(v as PlayMode)}
          segments={[
            { value: "mine", label: tr("play_mode_mine", undefined, "Play my PS5"), icon: MonitorPlay },
            // The stream bar's Play Together button has the same icon.
            { value: "together", label: tr("pt_title", undefined, "Play Together"), icon: Users },
            { value: "friend", label: tr("play_mode_friend", undefined, "Join a friend"), icon: LogIn },
          ]}
        />
      )}
      {mode === "friend" ? (
        <JoinAFriend pending={pendingJoin} onPendingDone={() => setPendingJoin(null)} />
      ) : (
        <StreamPanel together={mode === "together"} onTogether={onTogether} />
      )}
    </div>
  );
}
