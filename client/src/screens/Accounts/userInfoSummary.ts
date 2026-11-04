// The user-information backup: everything the app reads about a console's users and accounts,
// written as a text file a person can read and keep. Its main use is after a reset or a crash:
// a save is tied to the account ID, so a new user given the same ID (Accounts › Users) can take
// the old saves back. No secrets: the Remote Play keys and wake credentials are not in it.
import type { NpState, ProfileInfo, ProfileSlot } from "../../api/ps5";
import { accountIdToB64, accountIdToDecimal, formatAccountId } from "./accountId";
import { userSlots } from "./userSlots";
import { trStatic } from "../../lib/trStatic";

export interface UserInfoInput {
  at: Date;
  appVersion: string;
  consoleName: string;
  consoleAddr: string;
  payloadVersion: string | null;
  kernel: string | null;
  info: ProfileInfo;
  /** Each user's sign-in state, by user id, or why it could not be read. */
  np: Map<number, NpState | { error: string }>;
}

const yesNo = (v: boolean | null | undefined) => (v == null ? "unknown" : v ? "yes" : "no");

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** "2026-10-30 14:05" in local time. */
function stamp(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** The file's name: the console and the day, so backups of two consoles or two days differ. */
export function userInfoFileName(consoleName: string, at: Date): string {
  const name = consoleName.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") || "PS5";
  return `ps5upload-users-${name}-${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}.txt`;
}

function accountLines(slot: ProfileSlot | null, indent: string): string[] {
  if (!slot) return [`${indent}Account slot:        none found for this user`];
  const hex = formatAccountId(slot.id);
  const how = !slot.activated ? "not activated" : slot.offline_activated ? "activated offline" : "activated";
  return [
    `${indent}Account slot:        ${slot.slot}${slot.name.trim() ? ` ("${slot.name.trim()}")` : ""}`,
    `${indent}Account ID (hex):    ${hex}`,
    `${indent}Account ID (decimal): ${accountIdToDecimal(slot.id) || "—"}`,
    `${indent}Account ID (base64): ${accountIdToB64(slot.id) || "—"}`,
    `${indent}Account state:       ${how}`,
    `${indent}Account type:        ${slot.type || "—"} (flags 0x${slot.flags.toString(16)})`,
  ];
}

function npLines(np: NpState | { error: string } | undefined, indent: string): string[] {
  if (!np) return [];
  if ("error" in np) return [`${indent}Sign-in state:       could not be read (${np.error})`];
  return [
    `${indent}Online ID:           ${np.online_id || "—"}`,
    `${indent}Date of birth:       ${np.dob || "—"}`,
    `${indent}Age:                 ${np.age ?? "unknown"}`,
    `${indent}Sub-account:         ${np.sub_account == null ? "unknown" : np.sub_account ? "yes" : "no"}`,
    `${indent}Signed in before:    ${yesNo(np.auth_dat)} (auth data), ${yesNo(np.config_dat)} (config data)`,
    `${indent}Sign-in backup kept: ${np.backup ? `yes (slot ${np.backup_slot})` : "no"}`,
  ];
}

/** The whole file. */
export function userInfoSummary(i: UserInfoInput): string {
  const { info } = i;
  const slotOf = userSlots(info.users, info.slots);
  const used = new Set([...slotOf.values()].map((s) => s.slot));
  const others = info.slots.filter((s) => !used.has(s.slot) && (s.name.trim() || s.activated));
  const rule = "=".repeat(72);
  const out: string[] = [];

  out.push(trStatic("user_info_title", "PS5Upload: user information backup"), rule, "");
  out.push(`Made:           ${stamp(i.at)}`);
  out.push(`PS5Upload:      ${i.appVersion || "unknown"}`);
  out.push(`Console:        ${i.consoleName}${i.consoleAddr ? ` (${i.consoleAddr})` : ""}`);
  if (i.payloadVersion) out.push(`Helper:         ${i.payloadVersion}`);
  if (i.kernel) out.push(`Kernel:         ${i.kernel}`);
  const fg = info.users.find((u) => u.uid === info.uid);
  out.push(`Signed in now:  ${fg ? `${fg.username} (user ${info.uid_hex})` : info.username || "nobody"}`);
  out.push(`Users:          ${info.users.length}`, "");
  out.push(
    trStatic("user_info_private", "Keep this file private: an account ID identifies your account."),
    "",
  );

  out.push(rule, trStatic("user_info_users", "USERS"), rule, "");
  if (info.users.length === 0) {
    out.push(trStatic("user_info_no_users", "No users were found on this console."), "");
  }
  info.users.forEach((u, n) => {
    out.push(`${n + 1}. ${u.username || "(no name)"}${u.uid === info.uid ? "   [signed in now]" : ""}`);
    out.push(`   User ID:             ${u.uid_hex} (${u.uid})`);
    out.push(...accountLines(slotOf.get(u.uid) ?? null, "   "));
    out.push(...npLines(i.np.get(u.uid), "   "));
    out.push("");
  });

  if (others.length > 0) {
    out.push(
      rule,
      trStatic("user_info_other_slots", "OTHER ACCOUNT SLOTS (no user on this console goes with them)"),
      rule,
      "",
    );
    for (const s of others) {
      out.push(...accountLines(s, "   "), "");
    }
  }

  out.push(rule, trStatic("user_info_restoring", "RESTORING"), rule, "");
  out.push(
    trStatic("user_info_restore_tied", "Saves, trophies and licences are tied to the account ID, not to the user's name."),
    trStatic("user_info_restore_when", "After a console reset, a crash that lost a user, or on a new user:"),
    "",
    `  1. ${trStatic("user_info_restore_step1", "Add the user on the PS5 (Switch User > Add User) and sign in to it once.")}`,
    `  2. ${trStatic("user_info_restore_step2", "In PS5Upload, open Accounts > Users and find that user.")}`,
    `  3. ${trStatic("user_info_restore_step3", "Set its Account ID to the hex value above, for example 0x1234abcd, and activate it.")}`,
    `  4. ${trStatic("user_info_restore_step4", "Restore its saves from a backup (a game's page > Saves > Restore).")}`,
    "",
    trStatic("user_info_restore_same", "A save restores only to the same account ID and the same game."),
    "",
  );
  return out.join("\n");
}
