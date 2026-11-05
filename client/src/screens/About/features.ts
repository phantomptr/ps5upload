import {
  Cpu,
  Gamepad2,
  PackageOpen,
  PackagePlus,
  Radio,
  Save,
  Share2,
  Tv,
  Users,
  Video,
  WandSparkles,
  Zap,
} from "lucide-react";

/** Feature cards: one-sentence pitches of the app's main features, so a
 *  first-time visitor can scan "is this what I need?". Each carries a
 *  {key, fallback} pair because module-level constants can't reach `useTr`;
 *  the render loop calls `tr()` per card so language changes flow through. */
export const FEATURES: {
  icon: typeof Zap;
  titleKey: string;
  titleFallback: string;
  bodyKey: string;
  bodyFallback: string;
}[] = [
  {
    icon: Gamepad2,
    titleKey: "about_feat_play_title",
    titleFallback: "Play",
    bodyKey: "about_feat_play_body",
    bodyFallback:
      "Play the PS5 on your computer, phone or browser, with a controller, the keyboard and mouse, or on-screen controls.",
  },
  {
    icon: Users,
    titleKey: "about_feat_together_title",
    titleFallback: "Play Together",
    bodyKey: "about_feat_together_body",
    bodyFallback:
      "Friends join your game from a link as Player 2, 3 and 4, each with their own controller on the PS5.",
  },
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
    titleFallback: "Save Data",
    bodyKey: "about_feat_saves_body",
    bodyFallback:
      "Back saves up by themselves to a folder or a server, restore them, or copy them to another user or another PS5.",
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
  },
  {
    icon: Video,
    titleKey: "about_feat_golive_title",
    titleFallback: "Go live",
    bodyKey: "about_feat_golive_body",
    bodyFallback:
      "Stream your game to YouTube, Twitch, Kick, Facebook or any RTMP or SRT server, or add it to OBS.",
  },
  {
    icon: Tv,
    titleKey: "about_feat_minion_title",
    titleFallback: "PS5Upload Minion",
    bodyKey: "about_feat_minion_body",
    bodyFallback:
      "A page in the PS5's own browser for games, saves, files, cheats and more, made for the controller.",
  },
  {
    icon: Share2,
    titleKey: "about_feat_mesh_title",
    titleFallback: "Several devices, Mesh Drive",
    bodyKey: "about_feat_mesh_body",
    bodyFallback:
      "Computers, phones and Docker share one PS5 and its settings. Mesh Drive keeps a folder the same on all of them.",
  },
];
