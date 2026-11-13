// Share (12.3): open a link a friend shared, and what you have sent and received. Sharing your own
// saves, captures, cheats, controls or a homebrew app starts from that game's page (Share there);
// this screen is where a friend's link lands (ps5upload://receive, /share?receive=…).
//
// Layout (12.4.14): the same left-aligned column of cards as the other screens. The receive card
// on the left; how sharing works and the past shares beside it (below it on a narrow window).

import { useCallback, useEffect, useState } from "react";
import { useNavigate, useSearchParams } from "react-router";
import { Gamepad2, Inbox, Info } from "lucide-react";

import { Button, Card, PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import { ReceivePanel } from "./ReceivePanel";
import { ShareHistory } from "./ShareHistory";

export default function ShareScreen() {
  const tr = useTr();
  const [params, setParams] = useSearchParams();
  // A link handed over (an app link, the website's share page): taken into the form, then off the
  // address. Read on every change, so a second link opened while this screen shows is not lost.
  const [pending, setPending] = useState<string | null>(() => params.get("receive")?.trim() || null);
  useEffect(() => {
    if (!params.has("receive")) return;
    const next = params.get("receive")?.trim();
    if (next) setPending(next);
    setParams({}, { replace: true });
  }, [params, setParams]);
  // Read the past shares again each time a receive ends, so it shows up there.
  const [ends, setEnds] = useState(0);
  const onEnded = useCallback(() => setEnds((n) => n + 1), []);
  return (
    <div className="app-page space-y-4" data-testid="share-screen">
      <PageHeader
        icon={Inbox}
        title={tr("share_screen_title", undefined, "Share")}
        description={tr(
          "share_screen_desc",
          undefined,
          "Open a link a friend shared with you. To share your own saves, captures or a homebrew app, press Share on its game's page.",
        )}
      />
      <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(0,22rem)]">
        <ReceivePanel pending={pending} onEnded={onEnded} />
        <div className="space-y-4">
          <HowItWorks />
          <ShareHistory key={ends} variant="card" />
        </div>
      </div>
    </div>
  );
}

/** The three steps, and the way to the games to start one of your own. */
function HowItWorks() {
  const tr = useTr();
  const navigate = useNavigate();
  const steps = [
    tr("share_how_1", undefined, "On a game's page, press Share and pick what to send."),
    tr("share_how_2", undefined, "Send the link to your friend."),
    tr("share_how_3", undefined, "They open it here, see what you offer, and ask for it. Nothing moves until you allow it."),
  ];
  return (
    <Card
      title={tr("share_how_title", undefined, "How sharing works")}
      icon={Info}
      actions={
        <Button variant="secondary" size="sm" leftIcon={<Gamepad2 size={14} aria-hidden />}onClick={() => navigate("/games")} data-testid="share-go-games">
          {tr("share_how_games", undefined, "Choose a game")}
        </Button>
      }
    >
      <ol className="space-y-3" data-testid="share-how">
        {steps.map((s, i) => (
          <li key={i} className="flex gap-3 text-sm">
            <span
              aria-hidden
              className="grid h-6 w-6 shrink-0 place-items-center rounded-full bg-[var(--color-surface-3)] text-xs font-semibold tabular-nums text-[var(--color-muted)]"
            >
              {i + 1}
            </span>
            <span className="min-w-0 pt-0.5 text-[var(--color-muted)]">{s}</span>
          </li>
        ))}
      </ol>
    </Card>
  );
}
