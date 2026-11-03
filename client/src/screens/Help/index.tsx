import { HelpCircle, Info, Sparkles } from "lucide-react";

import TabbedShell, { type TabbedShellTab } from "../../layout/TabbedShell";
import { useTr } from "../../state/lang";
import AboutScreen from "../About";
import ChangelogScreen from "../Changelog";
import FAQScreen from "../FAQ";

type TabId = "faq" | "about" | "whats-new";

/**
 * Help (/faq): the FAQ, About and the full release notes as three tabs
 * (`?tab=faq|about|whats-new`). Each tab is the old screen's body, unchanged; this header
 * replaces theirs. The FAQ's own links (`?q=`, `?topic=`, `?item=`) open the FAQ tab.
 */
export default function HelpScreen() {
  const tr = useTr();
  const tabs: ReadonlyArray<TabbedShellTab<TabId>> = [
    {
      id: "faq",
      icon: HelpCircle,
      key: "faq",
      fallback: "FAQ",
      description: tr(
        "faq_description_v2",
        undefined,
        "How to set up, transfer, install and fix things. Search for a word or an error code, or pick a section.",
      ),
    },
    { id: "about", icon: Info, key: "about", fallback: "About" },
    {
      id: "whats-new",
      icon: Sparkles,
      key: "whats_new",
      fallback: "What's new",
      description: tr(
        "changelog_description_v2",
        undefined,
        "Release notes for every ps5upload version up to this one, bundled with the app. Newer releases and their downloads are on GitHub.",
      ),
    },
  ];

  return (
    <TabbedShell
      idPrefix="help"
      titleIcon={HelpCircle}
      titleKey="help_title"
      titleFallback="Help"
      tabs={tabs}
      renderPanel={(id) =>
        id === "about" ? (
          <AboutScreen embedded />
        ) : id === "whats-new" ? (
          <ChangelogScreen embedded />
        ) : (
          <FAQScreen embedded />
        )
      }
    />
  );
}
