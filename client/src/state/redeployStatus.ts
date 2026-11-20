// Why the reconnect loop (AppShell's useAutoRedeployDownHelpers) is not bringing a console's
// helper back right now, so "Helper isn't running" can say it instead of only "Send it again"
// (lib/autoRedeploy RedeployWhy). Rest mode and "stopped by you" have screens of their own.

import { create } from "zustand";

import { hostOf } from "../lib/addr";
import { MAX_REDEPLOYS_WITHOUT_RECOVERY, type RedeployWhy } from "../lib/autoRedeploy";
import type { Translator } from "./lang";

export const useRedeployStatusStore = create<{ byHost: Record<string, RedeployWhy> }>(() => ({ byHost: {} }));

/** Records (or with null, clears) why `host`'s helper is not being sent by itself. */
export function noteRedeployWhy(host: string, why: RedeployWhy | null): void {
  const key = hostOf(host) || host;
  const cur = useRedeployStatusStore.getState().byHost[key];
  if (why === null) {
    if (!cur) return;
    useRedeployStatusStore.setState((s) => {
      const next = { ...s.byHost };
      delete next[key];
      return { byHost: next };
    });
    return;
  }
  if (cur && JSON.stringify(cur) === JSON.stringify(why)) return;
  useRedeployStatusStore.setState((s) => ({ byHost: { ...s.byHost, [key]: why } }));
}

/** The reason for one console, reactively. */
export function useRedeployWhy(host: string | null | undefined): RedeployWhy | undefined {
  return useRedeployStatusStore((s) => (host ? s.byHost[hostOf(host) || host] : undefined));
}

/** One sentence for the "Helper isn't running" screens, or null when there is nothing to add. */
export function redeployWhyText(tr: Translator, why: RedeployWhy | undefined, now = Date.now()): string | null {
  if (!why) return null;
  switch (why.kind) {
    case "setting-off":
      return tr(
        "reconnect_why_setting_off",
        undefined,
        "Not sent by itself: Reconnect automatically is off for this PS5 (Settings for this PS5).",
      );
    case "busy":
      return tr(
        "reconnect_why_busy",
        undefined,
        "A transfer to this PS5 is still running, so the helper isn't sent over it. It is sent by itself once that ends.",
      );
    case "loader-closed":
      return tr(
        "reconnect_why_loader_closed",
        undefined,
        "Not sent by itself: the PS5's payload loader (port 9021) isn't answering, so nothing can take the helper. Load elfldr on the PS5 (your jailbreak or autoloader does it) and the helper follows by itself.",
      );
    case "settling":
      return tr(
        "reconnect_why_settling",
        { seconds: Math.max(1, Math.round((why.sendAt - now) / 1000)) },
        "The PS5's loader just came up. The helper is sent by itself in about {seconds} s, once the console has finished starting.",
      );
    case "alive":
      return tr(
        "reconnect_why_alive",
        undefined,
        "The helper still answers on its port, only not to this app. It gets a minute to recover before it is replaced.",
      );
    case "sending":
      return tr("helper_starting", undefined, "Starting the helper…");
    case "gave-up":
      return tr(
        "reconnect_why_gave_up",
        { count: MAX_REDEPLOYS_WITHOUT_RECOVERY, minutes: Math.max(1, Math.ceil((why.retryAt - now) / 60_000)) },
        "Sent {count} times without the helper starting. The next try is in about {minutes} min, or press Send helper now.",
      );
    case "answering":
      return tr(
        "reconnect_why_answering",
        undefined,
        "A helper is running on this PS5 but this app can't use it (most often another version of the app sent it). It is left running: press Send helper to replace it with this app's.",
      );
    case "limit":
      return limitText(tr, why.code, why.retryAt, now);
    case "console-busy":
      return tr(
        "reconnect_why_console_busy",
        undefined,
        "The PS5 is busy (an install, a transfer, an extraction or remote controls), and a new helper would stop that. It is sent by itself once that ends.",
      );
    case "stale-build":
      return tr(
        "reconnect_why_stale_build",
        undefined,
        "This app carries a helper of another version than itself, so it never sends it by itself. Reinstall the app; Send helper still sends it.",
      );
  }
}

/** Why unattended sends to a console stopped (the engine's refusal code). */
function limitText(tr: Translator, code: string, retryAt: number | null, now: number): string {
  if (code === "console_restarted_after_sends") {
    return tr(
      "reconnect_why_restarted",
      undefined,
      "The PS5 restarted twice right after the helper was sent to it by itself, so it is no longer sent by itself. Check the console, then press Send helper.",
    );
  }
  if (code === "different_console") {
    return tr(
      "reconnect_why_different_console",
      undefined,
      "Another PS5 answers at this address now (its network address changed), so the helper is not sent to it by itself. Check the address, then press Send helper.",
    );
  }
  return tr(
    "reconnect_why_budget",
    { minutes: Math.max(1, Math.ceil(((retryAt ?? now) - now) / 60_000)) },
    "The helper was sent several times in the last hour without staying up, so it is not sent by itself for about {minutes} min. Press Send helper to send it now.",
  );
}
