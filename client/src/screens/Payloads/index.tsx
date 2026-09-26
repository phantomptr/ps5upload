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

/**
 * Payloads screen — two URL-routed tabs:
 *
 *   - **catalog**: curated GitHub-released third-party homebrew.
 *   - **send**: arbitrary ELF/BIN/JS/LUA/JAR picker. Includes
 *     playlists and recent-sends history.
 *
 * (Historically split across /payloads and the old /send-payload route;
 * merged under ?tab=send for a cleaner sidebar. Legacy redirects remain
 * for old bookmarks.)
 *
 * The shell (URL contract + tablist + a11y + keyboard nav + page
 * header) lives in `layout/TabbedShell`; this file is just tab
 * metadata and a panel switch.
 */

type TabId = "catalog" | "send" | "shadowmount" | "nanodns";

export default function PayloadsScreen() {
  const tr = useTr();
  const host = useConnectionStore((state) => state.host);
  const tabs: ReadonlyArray<TabbedShellTab<TabId>> = [
    {
      id: "catalog",
      icon: Boxes,
      key: "payloads_tab_catalog",
      fallback: "Catalog",
      description: tr(
        "payloads_description_catalog",
        undefined,
        "Curated third-party PS5 homebrew payloads. Check for the latest release, download once, then send to your PS5 with one click. Versions cache locally so you can also bundle a USB autoloader stick.",
      ),
    },
    {
      id: "send",
      icon: Rocket,
      key: "payloads_tab_send",
      fallback: "Send file",
      description: tr(
        "payloads_description_send",
        undefined,
        "Send any PS5 payload file — .elf, .bin, .js, .lua, or .jar (kstuff, custom homebrew loaders, browser-stage exploits, plugin scripts, BD-JB JARs) — to your PS5. Same flow as the Connection tab, just pointed at a file you choose. Note: BD-JB-style .jar payloads need a JAR-aware loader on a non-9021 port — set the port to whatever your loader listens on.",
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

  const renderPanel = (id: TabId) => {
    if (id === "send") return <SendPanel />;
    if (id === "nanodns") return <NanoDnsScreen embedded />;
    if (id === "shadowmount") {
      return (
        <ConnectionGate require="payload">
          <SmpPanel
            mgmtAddr={host?.trim() ? mgmtAddr(host.trim()) : null}
            hideWhenUnavailable={false}
          />
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
