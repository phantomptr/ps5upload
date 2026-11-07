import { UserRound } from "lucide-react";

import { Button } from "../../../components";
import { useConfirm } from "../../../components/ConfirmDialog";
import { chosenRpUser, rpUsersOf } from "../../../lib/rpUsers";
import { useTr } from "../../../state/lang";
import { useRosterStore, type RpUser } from "../../../state/roster";

/** How a paired user is shown: the console's name for it, else "User N" (from 1). */
export function rpUserLabel(u: RpUser, i: number, tr: ReturnType<typeof useTr>): string {
  return u.name?.trim() || tr("rp_user_n", { n: i + 1 }, `User ${i + 1}`);
}

/**
 * The PS5 users this console is paired as (one pairing per user): which one Start uses, and a way
 * to forget one. Pairing again while another user is signed in on the PS5 adds that user.
 */
export function PairedUsers({ profileId }: { profileId: string }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const profile = useRosterStore((s) => s.profiles.find((p) => p.id === profileId) ?? null);
  const remove = useRosterStore((s) => s.removeRpUser);
  const users = rpUsersOf(profile);
  const chosen = chosenRpUser(profile);
  if (!users.length) return null;

  const forget = async (u: RpUser, label: string) => {
    const ok = await confirm({
      title: tr("rp_users_remove_confirm", { name: label }, `Forget the pairing for ${label}?`),
      message: tr(
        "rp_users_remove_body",
        undefined,
        "Play can no longer start as this user until you pair it again. The PS5 keeps this app on its list of linked devices until you remove it there.",
      ),
      confirmLabel: tr("rp_users_remove", undefined, "Forget"),
      destructive: true,
    });
    if (ok) remove(profileId, u.wake_regist_key);
  };

  return (
    <div className="space-y-1.5" data-testid="rp-paired-users">
      <p className="text-xs font-medium text-[var(--color-text)]">
        {tr("rp_users_title", undefined, "Paired PS5 users")}
      </p>
      <ul className="space-y-1">
        {users.map((u, i) => {
          const label = rpUserLabel(u, i, tr);
          return (
            <li
              key={u.wake_regist_key}
              className="flex items-center gap-2 rounded-lg border border-[var(--color-border)] px-2 py-1 text-xs"
              data-testid="rp-paired-user"
            >
              <UserRound size={14} aria-hidden />
              <span className="min-w-0 flex-1 truncate text-[var(--color-text)]">{label}</span>
              {u.wake_regist_key === chosen && (
                <span className="shrink-0 text-[var(--color-good)]">
                  {tr("rp_users_chosen", undefined, "Plays as this user")}
                </span>
              )}
              <Button size="sm" variant="ghost" onClick={() => void forget(u, label)} data-testid="rp-paired-user-remove">
                {tr("rp_users_remove", undefined, "Forget")}
              </Button>
            </li>
          );
        })}
      </ul>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_users_add_hint",
          undefined,
          "To play as another user too, sign in to that user on the PS5 and pair again: each user is kept, and Play › Start lets you pick one.",
        )}
      </p>
      {dialog}
    </div>
  );
}
