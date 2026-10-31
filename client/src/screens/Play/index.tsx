import { ListChecks, MonitorPlay } from "lucide-react";
import { useEffect } from "react";
import { useNavigate, useSearchParams } from "react-router";

import { Button, PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import "../../state/playSetup";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useConnectionStore } from "../../state/connection";
import { useRosterStore } from "../../state/roster";
import { useStreamLaunch } from "./game/streamLaunch";
import { usePlaySetup } from "./setup/usePlaySetup";
import { COMPACT_QUERY, useMedia } from "./immersive";
import { StreamPanel } from "./StreamPanel";

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
  // /play?launch=<TITLE>[&name=…] (search, links): start that game and stream it; a console not
  // set up for Play goes to its setup instead.
  const [params, setParams] = useSearchParams();
  const launch = params.get("launch")?.trim().toUpperCase() ?? "";
  const launchName = params.get("name")?.trim() ?? "";
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
    useStreamLaunch.getState().request(host, { titleId: launch, titleName: launchName || launch });
  }, [launch, launchName, host, paired, navigate, setParams]);
  // On a phone the picture and its controller need the height: while playing, the header is
  // just the title.
  const lean = compact && streaming;
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
      <StreamPanel />
    </div>
  );
}
