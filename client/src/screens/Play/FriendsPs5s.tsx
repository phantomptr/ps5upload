import { useEffect, useState } from "react";
import { Send, Tv, UserMinus } from "lucide-react";

import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useFormatDate } from "../../lib/formatDate";
import { useTr } from "../../state/lang";
import { canSend, friendsApi, outcomeText, type FriendPs5 } from "./friendsPs5";

/**
 * "Friends' PS5s" (apps/play/docs/EASY_JOIN.md, way 5): friends who joined from PS5Upload Play
 * before. "Send invite" puts this console's invite on their TV while Play Together is open; Forget
 * drops their pass (no more Play again, no more invites). Shown only when there are any.
 */
export function FriendsPs5s({ addr, open }: { addr: string; open: boolean }) {
  const tr = useTr();
  const date = useFormatDate();
  const { confirm, dialog } = useConfirm();
  const [friends, setFriends] = useState<FriendPs5[]>([]);
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [noInbox, setNoInbox] = useState<ReadonlySet<string>>(() => new Set());

  const reload = () => friendsApi.list().then(setFriends);
  useEffect(() => {
    let alive = true;
    void friendsApi.list().then((f) => alive && setFriends(f));
    return () => {
      alive = false;
    };
  }, [addr]);

  if (!friends.length) return <>{dialog}</>;

  const nameOf = (f: FriendPs5) => f.name || tr("pt_friend_fallback", undefined, "Friend");
  const send = async (f: FriendPs5) => {
    setBusy(f.id);
    const o = await friendsApi.send(f.id, addr);
    setBusy(null);
    if (o === "no_inbox") setNoInbox((s) => new Set([...s, f.id]));
    if (o === "unknown") void reload();
    const text = outcomeText(tr, o, nameOf(f));
    setNotes((n) => ({ ...n, [f.id]: text ?? "" }));
  };
  const forget = async (f: FriendPs5) => {
    const name = nameOf(f);
    const ok = await confirm({
      title: tr("pt_friends_forget_title", { name }, `Forget ${name}'s PS5?`),
      message: tr(
        "pt_friends_forget_body",
        undefined,
        "Their PS5 can't play again with you or get your invites anymore. They can still join with an invite link.",
      ),
      confirmLabel: tr("rp_users_remove", undefined, "Forget"),
      destructive: true,
    });
    if (!ok) return;
    await friendsApi.forget(f.id);
    await reload();
  };

  return (
    <section className="space-y-2 rounded-lg bg-[var(--color-surface)] p-3" data-testid="pt-friends">
      <h4 className="flex items-center gap-2 text-sm font-semibold">
        <Tv size={14} className="shrink-0" />
        {tr("pt_friends_title", undefined, "Friends' PS5s")}
      </h4>
      <ul className="space-y-2">
        {friends.map((f) => (
          <li key={f.id} className="space-y-1" data-testid="pt-friend">
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">{nameOf(f)}</span>
                {f.lastUnix > 0 && (
                  <span className="block text-xs text-[var(--color-muted)]">
                    {tr("pt_friends_last", { date: date(f.lastUnix * 1000, "date") }, `Last played ${date(f.lastUnix * 1000, "date")}`)}
                  </span>
                )}
              </span>
              {canSend(f, open, noInbox) && (
                <Button
                  size="sm"
                  variant="primary"
                  leftIcon={<Send size={14} />}
                  disabled={busy !== null}
                  onClick={() => void send(f)}
                  data-testid="pt-friend-send"
                >
                  {tr("pt_friends_send", undefined, "Send invite")}
                </Button>
              )}
              <Button
                size="sm"
                variant="ghost"
                leftIcon={<UserMinus size={14} />}
                onClick={() => void forget(f)}
                data-testid="pt-friend-forget"
              >
                {tr("rp_users_remove", undefined, "Forget")}
              </Button>
            </div>
            {notes[f.id] && (
              <p className="text-xs text-[var(--color-muted)]" data-testid="pt-friend-note">
                {notes[f.id]}
              </p>
            )}
          </li>
        ))}
      </ul>
      {dialog}
    </section>
  );
}
