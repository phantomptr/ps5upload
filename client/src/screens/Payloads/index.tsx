import { Boxes, Gamepad2, Rocket } from "lucide-react";
import { Navigate, useSearchParams } from "react-router";

import TabbedShell, { type TabbedShellTab } from "../../layout/TabbedShell";
import { useTr } from "../../state/lang";
import CatalogPanel from "./CatalogPanel";
import SendPanel from "./SendPanel";
import BrowserSendPanel from "./BrowserSendPanel";
import OnPs5Panel from "./hub/OnPs5Panel";
import { useConnectionStore } from "../../state/connection";
import { isTauriEnv } from "../../lib/tauriEnv";
import {
  defaultPayloadTab,
  legacyPayloadsTarget,
  payloadTabsFor,
  type PayloadTabId,
} from "./payloadTabs";

/**
 * Payloads screen — URL-routed tabs:
 *
 *   - **catalog**: curated GitHub-released third-party homebrew.
 *   - **send**: arbitrary ELF/BIN/JS/LUA/JAR picker. Includes
 *     playlists and recent-sends history.
 *   - **ps5** ("On your PS5"): a card per payload running or installed on the
 *     console; ShadowMount+, nanoDNS, Payload Manager and HearBridge open
 *     their own panel from the card. Available in the browser and native apps.
 *
 * The integrations used to be tabs of their own; `?tab=manager|shadowmount|
 * nanodns` now opens their card, and `?tab=ports` moved to Processes.
 */

type TabId = PayloadTabId;

export default function PayloadsScreen() {
  const tr = useTr();
  const [params] = useSearchParams();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  // "Connected" for picking the default tab: a console is set and not known to be down.
  // (Waiting for "up" would flip a bare /payloads from Catalog to On your PS5 mid-look.)
  const connected = !!host?.trim() && payloadStatus !== "down";
  const native = isTauriEnv();
  const legacy = legacyPayloadsTarget(params.get("tab"));
  if (legacy) return <Navigate to={legacy} replace />;

  const available = payloadTabsFor(native);
  const allTabs: ReadonlyArray<TabbedShellTab<TabId>> = [
    {
      id: "catalog",
      icon: Boxes,
      key: "payloads_tab_catalog",
      fallback: "Catalog",
      description: tr(
        "payloads_description_catalog_v2",
        undefined,
        "Curated third-party PS5 homebrew payloads. Check for the latest release, download once, then send to your PS5 with one click. Versions cache locally so you can also put them on a USB payload stick.",
      ),
    },
    {
      id: "send",
      icon: Rocket,
      key: "payloads_tab_send",
      fallback: "Send payload",
      description: !native ? tr("browserPayload.hint", "Send an ELF payload from this device through the server to the selected PS5. Start the ELF loader first. Sending bytes does not confirm that the payload started.") : tr(
        "payloads_description_send_v2",
        undefined,
        "Send any PS5 payload file — .elf, .bin, .js, .lua, or .jar (kstuff, custom homebrew loaders, browser-stage exploits, plugin scripts, BD-JB JARs) — to your PS5. Same flow as the Connection screen, just pointed at a file you choose. The port follows the file type (.elf/.bin 9021, .jar 9025, .lua 9026, .js 50000); change it only if your loader listens elsewhere.",
      ),
    },
    {
      id: "ps5",
      icon: Gamepad2,
      key: "payloads_tab_ps5",
      fallback: "On your PS5",
      description: tr(
        "payloads_description_ps5",
        undefined,
        "The payloads running or installed on the selected PS5. Open a payload's web page, update it, stop it, or manage it.",
      ),
    },
  ];
  const tabs = allTabs.filter((t) => available.includes(t.id));

  const renderPanel = (id: TabId) => {
    if (id === "send") return native ? <SendPanel /> : <BrowserSendPanel />;
    if (id === "ps5") return <OnPs5Panel />;
    return <CatalogPanel />;
  };

  return (
    <TabbedShell
      idPrefix="payloads"
      titleIcon={Boxes}
      titleKey="payloads"
      titleFallback="Payloads"
      tabs={tabs}
      defaultTab={defaultPayloadTab(native, connected)}
      renderPanel={renderPanel}
    />
  );
}
