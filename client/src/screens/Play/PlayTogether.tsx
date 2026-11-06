import { writeClipboard } from "../../lib/clipboard";
import { useEffect, useRef, useState } from "react";
import { Check, Copy, Gamepad2, RefreshCw, Share2, Smartphone, UserMinus, Users, X } from "lucide-react";

import { Button, Callout, Toggle } from "../../components";
import { SendHelperButton } from "../../components/SendHelperButton";
import { delayTone, formatCode, playerColor } from "../../lib/joinProtocol";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";
import { PictureHereToggle, PlayOnTvNote } from "./PictureHere";
import { QrCode } from "./PhonePads";
import {
  inviteOn,
  joinApi,
  playerSlots,
  slowFriend,
  streamSeatFriend,
  type JoinGuest,
  type JoinHostState,
} from "./playTogetherState";
import { NetworkChoice } from "./NetworkChoice";
import { clock } from "../../lib/clockText";

type Tr = ReturnType<typeof useTr>;

/** What a friend is doing, in words. */
export function guestStatusText(tr: Tr, g: JoinGuest): string {
  switch (g.status) {
    case "asking":
      return tr("pt_status_asking", undefined, "Asking");
    case "picking_user":
      return tr("pt_status_picking", undefined, "Picking a user on the TV");
    case "reconnecting": {
      const time = clock(g.reconnectingLeftS ?? 0);
      return tr("pt_status_reconnecting", { time }, `Reconnecting ${time}`);
    }
    case "left":
      return tr("pt_status_left", undefined, "Left");
    default:
      return tr("pt_status_playing", undefined, "Playing");
  }
}

const TONE = {
  good: "text-[var(--color-good)]",
  fair: "text-[var(--color-warn)]",
  slow: "text-[var(--color-bad)]",
  none: "text-[var(--color-muted)]",
} as const;

const CHIP: Record<JoinGuest["status"], string> = {
  asking: "bg-[var(--color-warn-soft)] text-[var(--color-warn)]",
  picking_user: "bg-[var(--color-warn-soft)] text-[var(--color-warn)]",
  playing: "bg-[var(--color-good-soft)] text-[var(--color-good)]",
  reconnecting: "bg-[var(--color-warn-soft)] text-[var(--color-warn)]",
  left: "bg-[var(--color-surface-3)] text-[var(--color-muted)]",
};

const rgb = (c: [number, number, number]) => `rgb(${c[0]},${c[1]},${c[2]})`;

function Step({ n, title, children }: { n: number; title: string; children: React.ReactNode }) {
  return (
    <section className="min-w-0 flex-1 space-y-2 rounded-lg bg-[var(--color-surface)] p-3" data-testid={`pt-step-${n}`}>
      <h4 className="flex items-center gap-2 text-sm font-semibold">
        <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[var(--color-accent)] text-xs font-bold text-[var(--color-on-accent,#fff)]">
          {n}
        </span>
        {title}
      </h4>
      {children}
    </section>
  );
}

/** The TV, as the friend will see it: "Who's using this controller?" with Quick Play picked. */
function TvHint() {
  const tr = useTr();
  return (
    <div className="mx-auto w-full max-w-[220px]" aria-hidden>
      <div className="rounded-md border-4 border-neutral-700 bg-gradient-to-b from-slate-800 to-slate-950 p-2 text-white">
        <p className="mb-2 text-center text-[10px] text-white/80">{tr("pt_tv_who", undefined, "Who's using this controller?")}</p>
        <div className="flex justify-center gap-1.5">
          <span className="h-7 w-7 rounded-full bg-white/20" />
          <span className="h-7 w-7 rounded-full bg-white/20" />
          <span className="flex h-7 items-center rounded-full bg-white px-2 text-[9px] font-semibold text-black ring-2 ring-sky-400">
            {tr("pt_tv_quick", undefined, "Quick Play")}
          </span>
        </div>
      </div>
      <div className="mx-auto h-1.5 w-12 rounded-b bg-neutral-700" />
    </div>
  );
}

/** A friend's name, or "Friend" in this app's language when they gave none. */
function friendName(tr: ReturnType<typeof useTr>, name: string): string {
  return name || tr("pt_friend_fallback", undefined, "Friend");
}

function PlayerCard({ addr, player, guest }: { addr: string; player: number; guest: JoinGuest | null }) {
  const tr = useTr();
  const tone = delayTone(guest?.rttMs);
  return (
    <li
      className={`flex min-w-0 flex-1 basis-40 flex-col gap-1.5 rounded-lg border p-3 ${
        guest && guest.status !== "left" ? "border-[var(--color-border)] bg-[var(--color-surface)]" : "border-dashed border-[var(--color-border)]"
      }`}
      data-testid={`pt-player-${player}`}
    >
      <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
        <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ background: rgb(playerColor(player)) }} />
        {tr("pt_player", { n: player }, `Player ${player}`)}
      </div>
      {guest ? (
        <>
          <div className="truncate text-sm font-semibold" data-testid="pt-player-name">
            {friendName(tr, guest.name)}
          </div>
          <div className="flex flex-wrap items-center gap-1.5">
            <span className={`rounded-full px-2 py-0.5 text-xs font-medium ${CHIP[guest.status]}`} data-testid="pt-player-status">
              {guestStatusText(tr, guest)}
            </span>
            {guest.rttMs != null && guest.status !== "left" && (
              <span className={`text-xs tabular-nums ${TONE[tone]}`} data-testid="pt-player-delay" data-tone={tone}>
                {Math.round(guest.rttMs)} ms
              </span>
            )}
            {guest.behind && guest.status === "playing" && (
              <span className="text-xs text-[var(--color-warn)]" data-testid="pt-player-behind">
                {tr("pt_player_behind", undefined, "Connection can't keep up")}
              </span>
            )}
          </div>
          {guest.status !== "left" && (
            <Button
              size="sm"
              variant="ghost"
              leftIcon={<UserMinus size={14} />}
              onClick={() => void joinApi.kick(addr, player)}
              className="self-start"
              data-testid="pt-kick"
            >
              {tr("pt_kick", undefined, "Remove")}
            </Button>
          )}
        </>
      ) : (
        <div className="text-sm text-[var(--color-muted)]">{tr("pt_free", undefined, "Free")}</div>
      )}
    </li>
  );
}

/**
 * Play Together (#641): friends play along on this PS5 as Player 2, 3 and 4 from their own
 * device. Three steps (invite, allow, pick Quick Play on the TV), the friends playing, and the
 * way to stop. Phones as this computer's own controller and watchers stay in their panel (link).
 */
export function PlayTogetherPanel({
  addr,
  state,
  onState,
  onClose,
  onPhones,
}: {
  /** The console being played. */
  addr: string;
  state: JoinHostState;
  onState: (s: JoinHostState) => void;
  onClose: () => void;
  /** Open the phones panel (a phone as your controller, or to watch). */
  onPhones: () => void;
}) {
  const tr = useTr();
  const pictureHere = useRemotePlayStore((s) => s.pictureHere);
  const [failed, setFailed] = useState(false);
  // Why opening failed (`old_helper` shows the update callout; others are named in the text).
  const [why, setWhy] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [picked, setPicked] = useState<string | null>(null);
  // A new link the engine would not make (shown, never swallowed: #657).
  const [linkFailed, setLinkFailed] = useState(false);
  const inviting = useRef(false);
  /** Asks the engine for the invite; `renew` voids the old link and makes a new one. */
  const askInvite = async (renew: boolean): Promise<void> => {
    if (inviting.current) return;
    inviting.current = true;
    try {
      const s = await joinApi.invite(addr, renew);
      setLinkFailed(!s);
      if (s) onState(s);
    } finally {
      inviting.current = false;
    }
  };

  // Opening the panel opens Play Together with a fresh invite (the engine keeps an open one).
  useEffect(() => {
    let alive = true;
    void (async () => {
      const reply = await joinApi.open(addr);
      if (!alive) return;
      if (!reply.state) {
        setWhy(reply.error);
        setFailed(reply.error !== "old_helper");
        return;
      }
      const opened = reply.state;
      setWhy(null);
      setFailed(false);
      onState(opened);
      if (!opened.invite) {
        const s = await joinApi.invite(addr);
        if (alive && s) onState(s);
      }
    })();
    return () => {
      alive = false;
    };
  }, [addr, onState]);

  const helperOk = why !== "old_helper";
  // An invite lasts 30 minutes: when it lapses while the panel is open, make the next one
  // (otherwise the panel showed "…" with no link and no way to get one).
  const lapsed = state.open && !state.invite && helperOk && !failed;
  useEffect(() => {
    if (lapsed) void askInvite(false);
    // askInvite reads the latest addr/onState; only a lapse starts it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [lapsed]);
  const invite = state.invite;
  const addresses = invite?.urls ?? [];
  const link = invite ? (picked ? inviteOn(invite.url, picked) : invite.url) : null;
  const slow = slowFriend(state);
  // The system share sheet (Messages, WhatsApp…), where the device has one: phones mostly.
  const canShare = typeof navigator !== "undefined" && typeof navigator.share === "function";
  const share = async () => {
    if (!link) return;
    try {
      await navigator.share({ title: tr("pt_button", undefined, "Play Together"), text: tr("pt_share_text", undefined, "Join my game on my PS5"), url: link });
    } catch {
      // Closed without sharing.
    }
  };
  const copy = async () => {
    if (!link) return;
    // writeClipboard also works on plain http, where the browser has no navigator.clipboard.
    if (await writeClipboard(link)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    }
  };

  return (
    <div
      className="space-y-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
      data-testid="pt-panel"
    >
      <div className="flex items-center gap-2">
        <Users size={16} />
        <h3 className="flex-1 text-sm font-semibold">{tr("pt_title", undefined, "Play together")}</h3>
        <button
          type="button"
          onClick={onClose}
          aria-label={tr("rp_phone_close", undefined, "Close")}
          className="rounded p-1 hover:bg-[var(--color-surface-3)]"
        >
          <X size={16} />
        </button>
      </div>
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "pt_intro",
          undefined,
          "Friends play along on your PS5 as Player 2, 3 and 4, each with their own controller, from their own phone, tablet or computer.",
        )}
      </p>

      {!helperOk && (
        <div data-testid="pt-helper-old">
          <Callout
            tone="warn"
            title={tr("pt_helper_old", undefined, "Update the helper to play together")}
            action={<SendHelperButton size="sm" />}
          >
            {tr(
              "pt_helper_old_body",
              undefined,
              "The helper running on the PS5 is older than Play Together. Send the new one, then open this again.",
            )}
          </Callout>
        </div>
      )}
      {failed && (
        <p className="text-sm text-[var(--color-warn)]" data-testid="pt-failed">
          {why === "host_not_local"
            ? tr(
                "pt_host_not_local",
                undefined,
                "A reverse proxy on the engine's computer is in the way. On the engine, set PS5UPLOAD_ALLOW_HOST to this page's name, and PS5UPLOAD_TRUSTED_PROXY to the proxy's address.",
              )
            : tr("pt_unavailable", undefined, "Play Together could not start. Check that the engine is up to date.")}
          {why && why !== "host_not_local" ? ` (${why})` : ""}
        </p>
      )}

      {helperOk && !failed && (
        <PictureHereToggle
          hint={tr(
            "pt_picture_hint",
            undefined,
            "Off, this device only invites your friends: no picture, sound or controller here, and your friends' game carries on. For playing on your TV.",
          )}
        />
      )}
      {helperOk && !failed && !pictureHere && <PlayOnTvNote />}
      {helperOk && !failed && pictureHere && state && streamSeatFriend(state) && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="pt-stream-seat-note">
          {tr(
            "pt_stream_seat_note",
            undefined,
            "A friend is using this stream's controller, so your controller here doesn't play until they leave.",
          )}
        </p>
      )}
      {helperOk && !failed && (
        <div className="flex flex-col gap-3 lg:flex-row">
          <Step n={1} title={tr("pt_step_invite", undefined, "Invite")}>
            {link ? (
              <div className="space-y-2">
                <div className="flex justify-center">
                  <QrCode text={link} size={160} testId="pt-qr" label={tr("pt_qr_label", undefined, "QR code to join the game")} />
                </div>
                <p className="text-xs text-[var(--color-muted)]">
                  {tr("pt_step_invite_body", undefined, "Your friend scans this with their phone's camera, or opens the link.")}
                </p>
                <p className="break-all font-mono text-xs" data-testid="pt-url">
                  {link}
                </p>
                <div className="flex flex-wrap gap-2">
                  <Button
                    size="sm"
                    variant="primary"
                    leftIcon={copied ? <Check size={14} /> : <Copy size={14} />}
                    onClick={() => void copy()}
                    data-testid="pt-copy"
                  >
                    {copied ? tr("pt_copied", undefined, "Copied") : tr("pt_copy", undefined, "Copy link")}
                  </Button>
                  {canShare && (
                    <Button size="sm" variant="secondary" leftIcon={<Share2 size={14} />} onClick={() => void share()} data-testid="pt-share">
                      {tr("pt_share", undefined, "Share")}
                    </Button>
                  )}
                  <Button
                    size="sm"
                    variant="ghost"
                    leftIcon={<RefreshCw size={14} />}
                    onClick={() => void askInvite(true)}
                    data-testid="pt-new-link"
                  >
                    {tr("pt_new_link", undefined, "New link")}
                  </Button>
                </div>
                {linkFailed && (
                  <p className="text-xs text-[var(--color-warn)]" data-testid="pt-new-link-failed">
                    {tr("pt_new_link_failed", undefined, "A new link could not be made. Try again.")}
                  </p>
                )}
                {invite?.code && (
                  <div className="space-y-0.5">
                    <p className="flex items-baseline gap-2 text-xs text-[var(--color-muted)]">
                      {tr("pt_code_label", undefined, "Short code")}
                      <span className="font-mono text-lg font-semibold tracking-wider text-[var(--color-text)]" data-testid="pt-code">
                        {formatCode(invite.code)}
                      </span>
                    </p>
                    <p className="text-xs text-[var(--color-muted)]">
                      {tr("pt_code_hint", undefined, "A friend on the same network can type it in Play › Join a friend.")}
                    </p>
                  </div>
                )}
                <NetworkChoice
                  urls={addresses}
                  tailscale={invite?.tailscale ?? null}
                  current={link}
                  onPick={setPicked}
                  otherNetwork={tr("pt_other_network", undefined, "This computer is on more than one network: pick the one your friend is on.")}
                  testId="pt-addresses"
                  addressTestId="pt-address"
                />
              </div>
            ) : linkFailed ? (
              <div className="space-y-2">
                <p className="text-xs text-[var(--color-warn)]" data-testid="pt-new-link-failed">
                  {tr("pt_new_link_failed", undefined, "A new link could not be made. Try again.")}
                </p>
                <Button size="sm" variant="ghost" leftIcon={<RefreshCw size={14} />} onClick={() => void askInvite(true)} data-testid="pt-new-link">
                  {tr("pt_new_link", undefined, "New link")}
                </Button>
              </div>
            ) : (
              <p className="text-xs text-[var(--color-muted)]">…</p>
            )}
          </Step>
          <Step n={2} title={tr("pt_step_allow", undefined, "Allow")}>
            <p className="text-xs text-[var(--color-muted)]">
              {tr("pt_step_allow_body", undefined, "When a friend asks, Allow appears over the picture. Only people you allow can play.")}
            </p>
            {state.asks.length > 0 && (
              <ul className="space-y-1.5" data-testid="pt-asks">
                {state.asks.map((a) => (
                  <li key={a.id} className="flex flex-wrap items-center gap-2 text-sm">
                    <span className="min-w-0 flex-1 truncate font-medium">{friendName(tr, a.name)}</span>
                    <Button size="sm" variant="primary" onClick={() => void joinApi.decide(addr, a.id, true)}>
                      {tr("pt_allow", undefined, "Allow")}
                    </Button>
                    <Button size="sm" variant="ghost" onClick={() => void joinApi.decide(addr, a.id, false)}>
                      {tr("pt_deny", undefined, "No")}
                    </Button>
                  </li>
                ))}
              </ul>
            )}
          </Step>
          <Step n={3} title={tr("pt_step_tv", undefined, "On the TV")}>
            <TvHint />
            <p className="text-xs text-[var(--color-muted)]">
              {tr("pt_step_tv_body", undefined, "Your friend presses PS on their controller and picks Quick Play (or their own user).")}
            </p>
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "pt_step_tv_menus",
                undefined,
                "On the home screen only you control the menus; your friend's controller plays inside games.",
              )}
            </p>
          </Step>
        </div>
      )}

      {helperOk && !failed && (
        <ul className="flex flex-wrap gap-2" data-testid="pt-players">
          {playerSlots(state).map(({ player, guest }) => (
            <PlayerCard key={player} addr={addr} player={player} guest={guest} />
          ))}
        </ul>
      )}

      {slow && (
        <div data-testid="pt-slow">
          <Callout
            tone="warn"
            title={tr(
              "pt_slow",
              { name: friendName(tr, slow.name), ms: Math.round(slow.rttMs ?? 0) },
              `${friendName(tr, slow.name)}'s connection is slow (${Math.round(slow.rttMs ?? 0)} ms).`,
            )}
          >
            {tr(
              "pt_slow_body2",
              undefined,
              "Lower Play quality in Settings › Picture (720p or 30 fps) for a smoother picture for everyone.",
            )}
          </Callout>
        </div>
      )}

      {helperOk && !failed && (
        <div data-testid="pt-block">
          <Toggle
            checked={state.settings.blockSystemButtons}
            onChange={(on) => {
              onState({ ...state, settings: { ...state.settings, blockSystemButtons: on } });
              void joinApi.settings(addr, { block_system_buttons: on }).then((s) => s && onState(s));
            }}
            label={tr("pt_block", undefined, "Block PS & Create for friends")}
            hint={tr("pt_block_hint", undefined, "Friends can still press PS once to pick a user on the TV.")}
          />
        </div>
      )}

      <p className="flex items-start gap-2 text-xs text-[var(--color-muted)]" data-testid="pt-tailscale">
        <Gamepad2 size={14} className="mt-0.5 shrink-0" />
        {tr(
          "pt_tailscale",
          undefined,
          "Playing from somewhere else? Both of you join the same Tailscale network, then share the link.",
        )}
      </p>

      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="ghost" leftIcon={<Smartphone size={14} />} onClick={onPhones} data-testid="pt-phones">
          {tr("pt_phones_link", undefined, "Use a phone as your controller, or let someone watch")}
        </Button>
        {state.open && (
          <Button
            size="sm"
            variant="secondary"
            onClick={() => {
              void joinApi.close(addr).then((r) => r.state && onState(r.state));
              onClose();
            }}
            data-testid="pt-close"
          >
            {tr("pt_close", undefined, "Close Play Together")}
          </Button>
        )}
      </div>
    </div>
  );
}

/** A friend asking to play: shown over the picture, full screen included. */
export function JoinAsks({ addr, asks }: { addr: string | null; asks: JoinHostState["asks"] }) {
  const tr = useTr();
  if (!asks.length || !addr) return null;
  const a = asks[0];
  return (
    <div
      className="pointer-events-auto absolute left-1/2 top-28 z-40 flex -translate-x-1/2 flex-wrap items-center gap-2 rounded-full bg-black/85 px-4 py-2 text-sm text-white shadow-lg"
      data-testid="pt-ask"
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <Users size={14} />
      <span>{tr("pt_asks", { name: friendName(tr, a.name) }, `${friendName(tr, a.name)} wants to play with you`)}</span>
      <button
        type="button"
        className="rounded-full bg-white px-3 py-1 text-xs font-medium text-black"
        onClick={() => void joinApi.decide(addr, a.id, true)}
        data-testid="pt-allow"
      >
        {tr("pt_allow", undefined, "Allow")}
      </button>
      <button
        type="button"
        className="rounded-full bg-white/15 px-3 py-1 text-xs"
        onClick={() => void joinApi.decide(addr, a.id, false)}
        data-testid="pt-deny"
      >
        {tr("pt_deny", undefined, "No")}
      </button>
    </div>
  );
}
