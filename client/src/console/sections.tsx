import type { ReactNode } from "react";
import { ArchiveRestore, Cpu, Folder, Gamepad2, House, Repeat, Settings, Sparkles, type LucideIcon } from "lucide-react";
import { Cheats } from "./cheats/Cheats";
import { Convert } from "./convert/Convert";
import { Files } from "./files/Files";
import { GamesSection } from "./games/GamesSection";
import { Home } from "./home/Home";
import { Saves } from "./saves/Saves";
import { Settings as SettingsSection } from "./settings/Settings";
import type { ConsoleStatus } from "./shell/TopBar";
import { SystemSection } from "./system/SystemSection";

/** What a section is opened with, e.g. Files at {path: "/mnt/usb0"}. */
export type SectionParams = Record<string, string>;

/** What App hands every section. */
export interface SectionContext {
  /** The last /api/status answer (App polls it for the console chip). */
  status: ConsoleStatus | null;
  /** Read /api/status again now (after closing a game). */
  refreshStatus: () => void;
  /** Switch to section `id`, opened with `params`. */
  openSection: (id: string, params?: SectionParams) => void;
  /** Whether section `id` exists and reads params (e.g. Files at a path). */
  canOpen: (id: string) => boolean;
  /** Whether section `id` is registered (Home hides the Cheats shortcut until it is). */
  hasSection: (id: string) => boolean;
  /** The params this section was opened with, or null. */
  params: SectionParams | null;
  /** Open the Controls screen (Settings). */
  openControls?: () => void;
  /** Show the first-run tips again (Settings). */
  showTips?: () => void;
}

/** One entry of the Rail. Only implemented sections are listed. */
export interface Section {
  id: string;
  icon: LucideIcon;
  labelKey: string;
  render: (ctx: SectionContext) => ReactNode;
  /** It reads ctx.params, so other sections may open it at a place (Home's drive cards → Files). */
  acceptsParams?: boolean;
}

// Registry: Home, Games (installed titles, and the Library of games on shares and drives, #498), Saves (save backups on the console), Files, Convert, Cheats (the running game's), Fan, Processes, Clean up, Settings. Files opens at ctx.params.path
// (acceptsParams), so Home's drive cards open it at a drive.
export const SECTIONS: Section[] = [
  {
    id: "home",
    icon: House,
    labelKey: "cw_nav_home",
    render: (ctx) => <Home ctx={ctx} />,
  },
  {
    id: "games",
    icon: Gamepad2,
    labelKey: "cw_games_title",
    // Installed and the Library (games on shares and drives, #498) under one entry. Home's "USB
    // drive connected" opens the Library scanning (params.scan); params.view picks a view.
    render: (ctx) => <GamesSection key={`${ctx.params?.view ?? ""}${ctx.params?.scan ?? ""}`} ctx={ctx} />,
    acceptsParams: true,
  },
  {
    id: "saves",
    icon: ArchiveRestore,
    labelKey: "cw_saves_title",
    // Games' "Save backups" opens it at a game (params.title).
    render: (ctx) => <Saves key={ctx.params?.title ?? ""} ctx={ctx} />,
    acceptsParams: true,
  },
  {
    id: "files",
    icon: Folder,
    labelKey: "cw_nav_files",
    render: (ctx) => <Files ctx={ctx} />,
    acceptsParams: true,
  },
  {
    // Convert a game folder or image into another format on the console itself (convert/model.ts).
    // Opened at a source with params.path.
    id: "convert",
    icon: Repeat,
    labelKey: "cw_convert_title",
    render: (ctx) => <Convert key={ctx.params?.path ?? ""} ctx={ctx} />,
    acceptsParams: true,
  },
  {
    id: "cheats",
    icon: Sparkles,
    labelKey: "cw_cheats_title",
    render: () => <Cheats />,
  },
  {
    // Fan, Processes and Clean up, one entry with a switch (system/SystemSection).
    id: "system",
    icon: Cpu,
    labelKey: "cw_system_title",
    render: (ctx) => <SystemSection key={ctx.params?.view ?? ""} ctx={ctx} />,
    acceptsParams: true,
  },
  {
    id: "settings",
    icon: Settings,
    labelKey: "cw_nav_settings",
    render: (ctx) => <SettingsSection onOpenControls={ctx.openControls} onShowTips={ctx.showTips} />,
  },
];
