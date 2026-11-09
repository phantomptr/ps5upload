// Share (12.3): open a link a friend shared, and what you have sent and received. Sharing your own
// saves, captures, cheats, controls or a homebrew app starts from that game's page (Share there);
// this screen is where a friend's link lands (ps5upload://receive, /share?receive=…).

import { useEffect, useState } from "react";
import { useSearchParams } from "react-router";
import { Inbox } from "lucide-react";

import { PageHeader } from "../../components";
import { useTr } from "../../state/lang";
import { ReceivePanel } from "./ReceivePanel";

export default function ShareScreen() {
  const tr = useTr();
  const [params, setParams] = useSearchParams();
  // A link handed over (an app link, the website's share page): filled in once, then off the address.
  const [pending] = useState<string | null>(() => params.get("receive")?.trim() || null);
  useEffect(() => {
    if (params.has("receive")) setParams({}, { replace: true });
  }, [params, setParams]);
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
      <ReceivePanel pending={pending} />
    </div>
  );
}
