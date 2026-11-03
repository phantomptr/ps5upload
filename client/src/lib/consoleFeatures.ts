/**
 * What the console's add-ons are doing, for a bug report (ps5Snapshot): Hush, the cheats engine
 * and the console's saved network shares. All read through the helper (fs reads, the cheats
 * status RPC, the shares list), never FTP.
 *
 * States and counts only: never a share's password or user name, never a cheat file's text.
 */
import { cheatsStatus, fsPathExists } from "../api/ps5";
import { listConsoleShares } from "../api/consoleShares";
import { readConsoleText } from "./rpBlockers";
import {
  HUSH_CONFIG_PATH,
  HUSH_DIR,
  HUSH_STATE_PATH,
  parseHushConfig,
} from "../screens/Hush/hushConfig";

export interface HushSummary {
  /** /data/hush exists (Hush was installed at some point). */
  installed: boolean;
  enabled: boolean | null;
  mode: string | null;
  /** 0: holds the patch for the whole session ("hold mode"); otherwise seconds. */
  hold_s: number | null;
  /** state.txt is there only while Hush's patch is in place. */
  state_txt_present: boolean;
}

export interface CheatsSummary {
  enabled: boolean;
  game_running: boolean;
  game_title_id: string;
  patches_last: number;
  patches_total: number;
  held: boolean | null;
}

export interface ConsoleShareSummary {
  host: string;
  share: string;
  has_user: boolean;
  has_password: boolean;
}

export interface ConsoleFeatures {
  hush: HushSummary | null;
  cheats: CheatsSummary | null;
  /** null: not read, or a helper older than console shares. */
  console_shares: ConsoleShareSummary[] | null;
}

/** Runs one probe, recording its failure (ps5Snapshot's `step`). */
export type Step = <T>(name: string, fn: () => Promise<T>) => Promise<T | null>;

/** Hush on the console. A console without its folder answers `installed: false` and stops. */
export async function hushSummary(maddr: string): Promise<HushSummary> {
  if (!(await fsPathExists(maddr, HUSH_DIR))) {
    return { installed: false, enabled: null, mode: null, hold_s: null, state_txt_present: false };
  }
  const cfgText = await readConsoleText(maddr, HUSH_CONFIG_PATH);
  const cfg = cfgText === null ? null : parseHushConfig(cfgText);
  return {
    installed: true,
    enabled: cfg?.enabled ?? null,
    mode: cfg?.mode ?? null,
    hold_s: cfg?.holdS ?? null,
    state_txt_present: await fsPathExists(maddr, HUSH_STATE_PATH),
  };
}

export async function collectConsoleFeatures(
  host: string,
  maddr: string,
  taddr: string,
  step: Step,
): Promise<ConsoleFeatures> {
  const hush = await step("hush", () => hushSummary(maddr));
  const st = await step("cheats", () => cheatsStatus(taddr));
  const shares = await step("console_shares", () => listConsoleShares(host));
  return {
    hush,
    cheats: st
      ? {
          enabled: st.enabled,
          game_running: st.game_running,
          game_title_id: st.game_title_id,
          patches_last: st.patches_last,
          patches_total: st.patches_total,
          held: st.held ?? null,
        }
      : null,
    console_shares: shares
      ? shares.map((s) => ({
          host: s.host,
          share: s.share,
          has_user: !!s.user,
          has_password: s.has_password,
        }))
      : null,
  };
}
