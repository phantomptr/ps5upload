import {
  Cpu,
  PackageOpen,
  PackagePlus,
  Radio,
  Save,
  WandSparkles,
  Zap,
} from "lucide-react";

/** Feature cards: one-sentence pitches of the app's main features, so a
 *  first-time visitor can scan "is this what I need?". Each carries a
 *  {key, fallback} pair because module-level constants can't reach `useTr`;
 *  the render loop calls `tr()` per card so language changes flow through.
 *  `nativeOnly` cards are hidden in the browser build, which can't do them. */
export const FEATURES: {
  icon: typeof Zap;
  titleKey: string;
  titleFallback: string;
  bodyKey: string;
  bodyFallback: string;
  nativeOnly?: boolean;
}[] = [
  {
    icon: Zap,
    titleKey: "about_feat_fast_transfers_title",
    titleFallback: "Fast transfers",
    bodyKey: "about_feat_fast_transfers_body",
    bodyFallback:
      "AVA1 transfer protocol with BLAKE3 verification, encrypted sessions, resumable jobs and small-file packing. Uses your LAN flat-out.",
  },
  {
    icon: PackageOpen,
    titleKey: "about_feat_install_title",
    titleFallback: "Install packages",
    bodyKey: "about_feat_install_body",
    bodyFallback:
      "PS4 .pkg and PS5 fake packages (base, update, DLC) from this computer, a NAS share, a USB drive or a link. If the PS5 refuses one route, the app offers another.",
  },
  {
    icon: PackagePlus,
    titleKey: "about_feat_convert_title",
    titleFallback: "Convert games",
    bodyKey: "about_feat_convert_body",
    bodyFallback:
      "Turn a game folder or image into an installable package, or a folder into a game image that ShadowMount+ mounts.",
  },
  {
    icon: Save,
    titleKey: "about_feat_saves_title",
    titleFallback: "Save data",
    bodyKey: "about_feat_saves_body",
    bodyFallback:
      "Back a game's saves up to a .zip on this computer and restore them later.",
  },
  {
    icon: WandSparkles,
    titleKey: "about_feat_cheats_title",
    titleFallback: "Cheats",
    bodyKey: "about_feat_cheats_body",
    bodyFallback:
      "Pick a game, download cheats made for its version, then switch them on while you play.",
  },
  {
    icon: Cpu,
    titleKey: "about_feat_console_title",
    titleFallback: "Console control",
    bodyKey: "about_feat_console_body",
    bodyFallback:
      "Rest mode, restart, shut down and wake over the network, plus running processes, the fan curve and a hardware overview.",
  },
  {
    icon: Radio,
    titleKey: "about_feat_payloads_title",
    titleFallback: "Payloads",
    bodyKey: "about_feat_payloads_body",
    bodyFallback:
      "Send payload ELFs to the console's loader, from the catalogue or your own files, one at a time or as a playlist.",
    nativeOnly: true,
  },
];

/** The feature cards this build can honestly offer. */
export function visibleFeatures(native: boolean): typeof FEATURES {
  return FEATURES.filter((f) => native || !f.nativeOnly);
}
