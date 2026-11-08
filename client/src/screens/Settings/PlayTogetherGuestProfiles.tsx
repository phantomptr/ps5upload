import { useCallback, useEffect, useState } from "react";

import { Select } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import {
  joinApi,
  saveSeat,
  type ConsoleProfile,
  type ProfilesState,
} from "../Play/playTogetherState";

/** The guest players a profile can be chosen for. */
const PLAYERS = [2, 3, 4] as const;

/** Companion states in which the helper can't seat a guest in a profile. */
const CANT_SEAT = new Set(["no_bind", "unavailable", "off", "old_helper"]);

/**
 * Settings › Play Together › Guests play as: for Player 2, 3 and 4 on the connected console, the
 * PS5 profile a friend plays as. The helper seats their controller in it as it is made, so the TV
 * never asks "Who's using this controller?" and the friend can't pick another profile. "Let the TV
 * ask" keeps the old behaviour. Only profiles signed in on the PS5 are offered (signing one in
 * from here would skip its passcode). The host's own profile (the one in front) asks first: a
 * friend playing as it uses its saves and trophies.
 */
export function PlayTogetherGuestProfiles({ host }: { host: string }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [state, setState] = useState<ProfilesState | null>(null);
  const [unread, setUnread] = useState(false);
  const [why, setWhy] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    const p = host ? await joinApi.profiles(host) : null;
    setUnread(!p);
    if (p) setState(p);
    return p;
  }, [host]);

  useEffect(() => {
    let alive = true;
    void (async () => {
      const p = host ? await joinApi.profiles(host) : null;
      if (!alive) return;
      setUnread(!p);
      // Another console (or none read): never keep the last console's profiles on screen, or a
      // pick would send its user ids to this one.
      setState(p);
    })();
    return () => {
      alive = false;
    };
  }, [host]);

  const choose = async (player: number, value: string) => {
    if (!state) return;
    const user: ConsoleProfile | null = value ? (state.users.find((u) => String(u.id) === value) ?? null) : null;
    if (value && !user) return;
    let owner = false;
    if (user?.foreground) {
      owner = await confirm({
        title: tr("pt_set_owner_title", { name: user.name }, `Let a friend play as ${user.name}?`),
        message: tr(
          "pt_set_owner_body",
          undefined,
          "This is your own profile. A friend playing as it can use your saves and trophies.",
        ),
        confirmLabel: tr("pt_set_owner_yes", undefined, "Let them"),
        destructive: true,
      });
      if (!owner) return;
    }
    setBusy(true);
    // When the PS5 doesn't say which profile is the host's, this one may be: the same question.
    const r = await saveSeat(host, state.seats, player, user, owner, () =>
      confirm({
        title: tr("pt_set_owner_title", { name: user?.name ?? "" }, `Let a friend play as ${user?.name ?? ""}?`),
        message: tr(
          "pt_set_owner_unknown_body",
          undefined,
          "The PS5 didn't say which profile is yours. If this is your own profile, a friend playing as it can use your saves and trophies.",
        ),
        confirmLabel: tr("pt_set_owner_yes", undefined, "Let them"),
        destructive: true,
      }),
    );
    if (r.error === "cancelled") {
      setBusy(false);
      return;
    }
    setWhy(r.error);
    await load();
    setBusy(false);
  };

  if (unread && !state) {
    return (
      <p className="text-xs text-[var(--color-muted)]" data-testid="pt-set-guests-unread">
        {tr(
          "pt_set_guests_unread",
          undefined,
          "Couldn't read the PS5's profiles. Check that the PS5 is on and the helper is running.",
        )}
      </p>
    );
  }
  if (!state) return <p className="text-xs text-[var(--color-muted)]">…</p>;

  const label = (u: { name: string; foreground: boolean }) =>
    u.foreground ? tr("pt_set_owner_mark", { name: u.name }, `${u.name} (you)`) : u.name;

  return (
    <div className="space-y-3" data-testid="pt-set-guests">
      <div>
        <p className="text-sm font-medium text-[var(--color-text)]">
          {tr("pt_set_guests_as", undefined, "Guests play as")}
        </p>
        <p className="mt-0.5 text-xs text-[var(--color-muted)]">
          {tr(
            "pt_set_guests_hint",
            undefined,
            "Create profiles such as Guest 1 on the PS5 and sign them in to use them here",
          )}
        </p>
      </div>
      {PLAYERS.map((player) => {
        const seat = state.seats.find((s) => s.player === player) ?? null;
        const missing = seat && !state.users.some((u) => u.id === seat.user);
        return (
          <Select
            key={player}
            label={tr("pt_player", { n: player }, `Player ${player}`)}
            value={seat ? String(seat.user) : ""}
            disabled={busy}
            onChange={(e) => void choose(player, e.target.value)}
            data-testid={`pt-set-guest-${player}`}
          >
            <option value="">{tr("pt_set_tv_asks", undefined, "Let the TV ask")}</option>
            {state.users.map((u) => (
              <option key={u.id} value={String(u.id)}>
                {label(u)}
              </option>
            ))}
            {missing && (
              <option value={String(seat.user)}>
                {tr("pt_set_signed_out", { name: seat.name }, `${seat.name} (not signed in)`)}
              </option>
            )}
          </Select>
        );
      })}
      <p className="text-xs text-[var(--color-muted)]" data-testid="pt-set-guests-signed-in">
        {tr(
          "pt_set_guests_signed_in",
          undefined,
          "Only profiles signed in on the PS5 are listed. If a profile can't be used when a friend joins, the TV asks them who they are.",
        )}
      </p>
      {state.seats.length > 0 && state.companion && CANT_SEAT.has(state.companion) && (
        <p className="text-xs text-[var(--color-warn)]" data-testid="pt-set-guests-cant">
          {tr(
            "pt_set_guests_cant",
            undefined,
            "This PS5's helper can't give a friend a profile, so the TV asks them who they are. Update the helper.",
          )}
        </p>
      )}
      {why && (
        <p className="text-xs text-[var(--color-warn)]" data-testid="pt-set-guests-failed">
          {why === "owner_confirm"
            ? tr(
                "pt_set_owner_refused",
                undefined,
                "That profile is now in front on the PS5. Choose it again to confirm a friend may use it.",
              )
            : `${tr("pt_set_failed", undefined, "The engine didn't take this setting. Try again.")} (${why})`}
        </p>
      )}
      {dialog}
    </div>
  );
}
