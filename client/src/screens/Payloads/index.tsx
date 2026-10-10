import { Boxes, Globe, HardDrive, Rocket } from "lucide-react";

import TabbedShell, { type TabbedShellTab } from "../../layout/TabbedShell";
import { useTr } from "../../state/lang";
import CatalogPanel from "./CatalogPanel";
import SendPanel from "./SendPanel";
import NanoDnsScreen from "../NanoDns";
import SmpPanel from "../Library/SmpPanel";
import { ConnectionGate } from "../../components";
import { useConnectionStore } from "../../state/connection";
import { mgmtAddr } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import { payloadTabsFor, type PayloadTabId } from "./payloadTabs";

/**
 * Payloads screen — URL-routed tabs:
 *
 *   - **catalog**: curated GitHub-released third-party homebrew.
 *   - **send**: arbitrary ELF/BIN/JS/LUA/JAR picker. Includes
 *     playlists and recent-sends history.
 *   - **shadowmount** / **nanodns**: those payloads' status and config on
 *     the console. The only two the browser build shows (payloadTabsFor).
 *
 * (Historically split across /payloads and the old /send-payload route;
 * merged under ?tab=send for a cleaner sidebar. Legacy redirects remain
 * for old bookmarks.)
 *
 * The shell (URL contract + tablist + a11y + keyboard nav + page
 * header) lives in `layout/TabbedShell`; this file is just tab
 * metadata and a panel switch.
 */

type TabId = PayloadTabId;

export default function PayloadsScreen() {
  const tr = useTr();
  const host = useConnectionStore((state) => state.host);
  const available = payloadTabsFor(isTauriEnv());
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
      description: tr(
        "payloads_description_send_v2",
        undefined,
        "Send any PS5 payload file — .elf, .bin, .js, .lua, or .jar (kstuff, custom homebrew loaders, browser-stage exploits, plugin scripts, BD-JB JARs) — to your PS5. Same flow as the Connection screen, just pointed at a file you choose. The port follows the file type (.elf/.bin 9021, .jar 9025, .lua 9026, .js 50000); change it only if your loader listens elsewhere.",
      ),
    },
    {
      id: "shadowmount",
      icon: HardDrive,
      key: "payloads_tab_shadowmount",
      fallback: "ShadowMount+",
      description: tr(
        "payloads_description_shadowmount",
        undefined,
        "Inspect ShadowMount+ status, mounted game images, configuration, and diagnostics on the selected PS5.",
      ),
    },
    {
      id: "nanodns",
      icon: Globe,
      key: "payloads_tab_nanodns",
      fallback: "nanoDNS",
      description: tr(
        "payloads_description_nanodns",
        undefined,
        "Configure the nanoDNS payload, verify its running version, and apply safe config migrations.",
      ),
    },
  ];
  const tabs = allTabs.filter((t) => available.includes(t.id));

  const renderPanel = (id: TabId) => {
    if (id === "send") return <SendPanel />;
    if (id === "nanodns") return <NanoDnsScreen />;
    if (id === "shadowmount") {
      return (
        <ConnectionGate require="payload">
          <SmpPanel mgmtAddr={host?.trim() ? mgmtAddr(host.trim()) : null} />
        </ConnectionGate>
      );
    }
    return <CatalogPanel />;
  };

  return (
    <TabbedShell
      idPrefix="payloads"
      titleIcon={Boxes}
      titleKey="payloads"
      titleFallback="Payloads"
      tabs={tabs}
      renderPanel={renderPanel}
    />
  );
}
