import { useEffect } from "react";
import { Users } from "lucide-react";

import { Button, Modal } from "../components";
import { useConfirm } from "../components/ConfirmDialog";
import { friendsConfirmed, useFriendsGuard } from "../lib/friendsGuard";
import { friendsPlaying } from "../lib/rpStopConfirm";
import { isMobile } from "../lib/platform";
import { isTauriEnv } from "../lib/tauriEnv";
import { useConnectionStore } from "../state/connection";
import { useTr } from "../state/lang";

type Tr = ReturnType<typeof useTr>;

/** "Mia and Lee are playing…" in this language's list form. */
export function friendsList(names: string[], lang: string, fallback: string): string {
  if (!names.length) return fallback;
  try {
    const LF = (Intl as unknown as { ListFormat?: new (l: string, o: object) => { format: (x: string[]) => string } }).ListFormat;
    return LF ? new LF(lang, { style: "long", type: "conjunction" }).format(names) : names.join(", ");
  } catch {
    return names.join(", ");
  }
}

export function friendsTitle(tr: Tr, names: string[], lang: string): string {
  const who = friendsList(names, lang, tr("room_friends_generic", undefined, "Friends"));
  return tr("room_guard_title", { who }, `${who} are playing on this PS5. Stop anyway?`);
}

/**
 * The engine's "friends playing" guard asked through one dialog for the whole app (lib/friendsGuard
 * wraps fetch), and the desktop window's close while friends play: it asks, and the safe default
 * is to keep the room running (the window goes down to the Dock or taskbar; quitting would end
 * their game).
 */
export function FriendsGuardHost() {
  const tr = useTr();
  const ask = useFriendsGuard((s) => s.ask);
  const lang = typeof document !== "undefined" ? document.documentElement.lang || "en" : "en";
  const done = (ok: boolean) => {
    ask?.resolve(ok);
    useFriendsGuard.setState({ ask: null });
  };
  const closeDialog = useCloseWhileFriends();
  if (!ask) return <>{closeDialog}</>;
  return (
    <>
    {closeDialog}
    <Modal
      open
      onClose={() => done(false)}
      title={friendsTitle(tr, ask.friends, lang)}
      titleIcon={<Users size={16} />}
      size="sm"
      footer={
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="primary" onClick={() => done(false)} data-testid="friends-guard-keep">
            {tr("room_guard_keep", undefined, "Keep playing")}
          </Button>
          <Button variant="danger" onClick={() => done(true)} data-testid="friends-guard-stop">
            {tr("room_guard_stop", undefined, "Stop anyway")}
          </Button>
        </div>
      }
    >
      <p className="text-sm text-[var(--color-muted)]" data-testid="friends-guard">
        {tr("room_guard_body", undefined, "This ends their game. They can join again when you play again.")}
      </p>
    </Modal>
    </>
  );
}

/** The desktop window's close while friends play in this app's room: ask, default keep running. */
function useCloseWhileFriends() {
  const tr = useTr();
  const { choose, dialog } = useConfirm();
  useEffect(() => {
    if (!isTauriEnv() || isMobile()) return;
    let off: (() => void) | null = null;
    let alive = true;
    void import("@tauri-apps/api/window")
      .then(async ({ getCurrentWindow }) => {
        const win = getCurrentWindow();
        const un = await win.onCloseRequested(async (event) => {
          const host = useConnectionStore.getState().host;
          const n = await friendsPlaying(host).catch(() => 0);
          if (n === 0) return;
          event.preventDefault();
          const pick = await choose({
            title: tr("room_close_window_title", { n }, `${n} friend(s) are playing in your room.`),
            message: tr(
              "room_close_window_body",
              undefined,
              "Closing PS5Upload ends their game. Keep it running in the background instead?",
            ),
            confirmLabel: tr("room_close_window_go", undefined, "OK"),
            choices: [
              { value: "keep", label: tr("room_close_window_keep", undefined, "Keep running (recommended)") },
              { value: "quit", label: tr("room_close_window_quit", undefined, "Quit and end their game") },
            ],
          });
          if (pick === "quit") {
            friendsConfirmed();
            await win.destroy();
          } else if (pick === "keep") {
            await win.minimize();
          }
        });
        if (alive) off = un;
        else un();
      })
      .catch(() => {
        // No window API here: closing works as before.
      });
    return () => {
      alive = false;
      off?.();
    };
  }, [choose, tr]);
  return dialog;
}
