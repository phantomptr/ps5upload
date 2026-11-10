import { writeClipboard } from "../../lib/clipboard";
import { useEffect, useRef, useState } from "react";
import { Check, Copy, Gamepad2, RefreshCw, Share2, Smartphone, UserMinus, Users, X } from "lucide-react";

import { Button, Callout, Toggle } from "../../components";
import { SendHelperButton } from "../../components/SendHelperButton";
import { delayTone, formatCode, playerColor } from "../../lib/joinProtocol";
import { useTr } from "../../state/lang";
import { RP_BITRATES, useRemotePlayStore } from "../../state/remotePlay";
import { useRpQualityStore } from "../../state/rpQuality";
import { PictureHereToggle, PlayOnTvNote } from "./PictureHere";
import { QrCode } from "./PhonePads";
import {
  inviteOn,
  joinApi,
  linkGrade,
  lowerForAll,
  openWhenPlaying,
  playerSlots,
  shareOf,
  slowFriend,
  streamSeatFriend,
  type JoinGuest,
  type JoinHostState,
  type NetPeer,
} from "./playTogetherState";
import { InternetHint, netInviteNote, NetworkChoice, saveNetwork, savedNetwork, splitByNetwork, urlHost, type NetworkKind } from "./NetworkChoice";
import { clock } from "../../lib/clockText";

type Tr = ReturnType<typeof useTr>;

// Shared with the phone link's panel (PhonePads): it lives beside the network choice.
export { netInviteNote };

/** Internet friends not in the game: those still connecting, and whether one failed that the
 *  host has not dismissed (one notice, never a list). */
export function netPeersView(peers: NetPeer[], dismissed: ReadonlySet<string>): { connecting: NetPeer[]; failed: NetPeer[] } {
  return {
    connecting: peers.filter((p) => p.path === "connecting"),
    failed: peers.filter((p) => p.path === "failed" && !dismissed.has(p.gid)),
  };
}

/** How a friend is connected, in words (null: nothing to say, as for a LAN friend on an older
 *  engine). The engine's `link.path` from ICE's selected pair; an internet friend still
 *  connecting says so. */
export function guestWayText(tr: Tr, g: JoinGuest): string | null {
  if (g.via === "internet" && g.path === "connecting") return tr("pt_net_connecting", undefined, "Connecting…");
  switch (g.netPath) {
    case "lan":
      return tr("pt_way_lan", undefined, "Home network");
    case "tailscale":
      return tr("net_tailscale", undefined, "Tailscale");
    case "internet":
      return tr("pt_way_internet", undefined, "Internet direct");
  }
  // An older engine names only an internet friend's connection.
  if (g.via === "internet")
    return g.path === "direct" ? `${tr("net_internet", undefined, "Internet")} · ${tr("pt_path_direct", undefined, "Direct")}` : tr("net_internet", undefined, "Internet");
  return null;
}

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

/** Under an ask, when allowing it gives the friend this stream's controller (#720). */
function SeatNextNote({ name, className = "text-xs text-[var(--color-muted)]" }: { name: string; className?: string }) {
  const tr = useTr();
  return (
    <p className={className} data-testid="pt-ask-seat">
      {tr(
        "pt_ask_seat_stream",
        { name },
        `If you allow ${name}, they get this stream's controller as Player 1 and play as your PS5 user.`,
      )}
    </p>
  );
}

/** Under an ask, when allowing it seats the friend in the PS5 profile chosen for their player
 *  (Settings › Play Together › Guests play as). */
function ProfileNextNote({
  name,
  profile,
  className = "text-xs text-[var(--color-muted)]",
}: {
  name: string;
  profile: string;
  className?: string;
}) {
  const tr = useTr();
  return (
    <p className={className} data-testid="pt-ask-profile">
      {tr("pt_ask_plays_as", { name, profile }, `If you allow ${name}, they play as ${profile}.`)}
    </p>
  );
}

/** Why the profile chosen for a friend's player wasn't used (the TV asks them instead). */
function seatNoteText(tr: Tr, note: string): string {
  return note === "signed_out"
    ? tr("pt_seat_note_signed_out", undefined, "Their chosen profile isn't signed in on the PS5, so the TV asks who they are.")
    : tr("pt_seat_note", undefined, "Their chosen profile couldn't be used, so the TV asks who they are.");
}

function PlayerCard({ addr, player, guest }: { addr: string; player: number; guest: JoinGuest | null }) {
  const tr = useTr();
  const tone = delayTone(guest?.rttMs);
  const onSeat = !!guest && guest.streamSeat && guest.status !== "left";
  const way = guest ? guestWayText(tr, guest) : null;
  return (
    <li
      className={`flex min-w-0 flex-1 basis-40 flex-col gap-1.5 rounded-lg border p-3 ${
        guest && guest.status !== "left" ? "border-[var(--color-border)] bg-[var(--color-surface)]" : "border-dashed border-[var(--color-border)]"
      }`}
      data-testid={`pt-player-${player}`}
    >
      <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
        <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ background: rgb(playerColor(player)) }} />
        {onSeat
          ? tr("pt_player_stream_seat", undefined, "Player 1 – plays as your PS5 user")
          : tr("pt_player", { n: player }, `Player ${player}`)}
      </div>
      {guest ? (
        <>
          <div className="truncate text-sm font-semibold" data-testid="pt-player-name">
            {friendName(tr, guest.name)}
          </div>
          {onSeat && (
            <p className="text-xs text-[var(--color-muted)]" data-testid="pt-player-stream-seat">
              {tr(
                "pt_seat_stream",
                { name: friendName(tr, guest.name) },
                `${friendName(tr, guest.name)} is using this stream's controller and plays as your PS5 user`,
              )}
            </p>
          )}
          {guest.playsAs && guest.status !== "left" && (
            <p className="truncate text-xs text-[var(--color-muted)]" data-testid="pt-player-plays-as">
              {tr("pt_plays_as", { profile: guest.playsAs }, `Plays as ${guest.playsAs}`)}
            </p>
          )}
          {guest.seatNote && guest.status !== "left" && (
            <p className="text-xs text-[var(--color-warn)]" data-testid="pt-player-seat-note" data-note={guest.seatNote}>
              {seatNoteText(tr, guest.seatNote)}
            </p>
          )}
          <div className="flex flex-wrap items-center gap-1.5">
            <span className={`rounded-full px-2 py-0.5 text-xs font-medium ${CHIP[guest.status]}`} data-testid="pt-player-status">
              {guestStatusText(tr, guest)}
            </span>
            {way && guest.status !== "left" && (
              <span
                className="rounded-full bg-[var(--color-accent-soft)] px-2 py-0.5 text-xs font-medium"
                data-testid="pt-player-via"
                data-path={guest.path ?? ""}
                data-way={guest.netPath ?? ""}
              >
                {way}
              </span>
            )}
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
            {/* Their video track's estimate stays under the stream's bitrate: only a lower quality
                helps (the PS5 can't change it mid-stream). */}
            {guest.carries && guest.status === "playing" && (
              <span className="text-xs text-[var(--color-warn)]" data-testid="pt-player-carries">
                {tr(
                  "pt_player_carries",
                  { got: (guest.carries.kbps / 1000).toFixed(1), need: (guest.carries.streamKbps / 1000).toFixed(1) },
                  `Their connection carries ${(guest.carries.kbps / 1000).toFixed(1)} of ${(guest.carries.streamKbps / 1000).toFixed(1)} Mbit/s: lower the stream's quality`,
                )}
              </span>
            )}
            {/* The connection check (their video track's estimate, loss, round trip): a word and
                about how much it carries, live; under the rate, the line above says it. */}
            {(() => {
              const lg = guest.status === "playing" && !guest.carries ? linkGrade(guest.link) : null;
              if (!lg) return null;
              const mbps = lg.mbps ?? 0;
              const text =
                lg.grade === "measuring"
                  ? tr("pt_link_checking", undefined, "Checking the connection…")
                  : lg.grade === "good"
                    ? tr("pt_link_good", { mbps }, `Good connection, about ${mbps} Mbps`)
                    : lg.grade === "ok"
                      ? tr("pt_link_ok", { mbps }, `OK connection, about ${mbps} Mbps`)
                      : tr("pt_link_slow", { mbps }, `Slow connection, about ${mbps} Mbps`);
              return (
                <span
                  className={`text-xs ${lg.grade === "slow" ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}
                  data-testid="pt-player-link"
                  data-grade={lg.grade}
                >
                  {text}
                </span>
              );
            })()}
            {/* What their own page says: the picture stands still at times on their side. */}
            {!guest.behind && !guest.carries && guest.status === "playing" && (guest.quality === "stutters" || guest.quality === "poor") && (
              <span className="text-xs text-[var(--color-warn)]" data-testid="pt-player-stutter" data-quality={guest.quality}>
                {tr("pt_player_stutter", undefined, "Their picture stutters")}
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
  focusOnOpen,
}: {
  /** The console being played. */
  addr: string;
  /** Set (Start and invite): the panel takes the keyboard focus when it shows, once. */
  focusOnOpen?: { current: boolean };
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
  // The home network's address the Wi-Fi link is for, on a computer with more than one (this
  // device remembers the last one picked: "tailscale" or the home network).
  const [network, setNetwork] = useState<NetworkKind>(savedNetwork);
  // Internet friends whose failed connection the host dismissed.
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(() => new Set());
  // An engine from before internet invites says nothing of them: the home network's link only.
  const netOffered = state.settings.internetInvites !== null;
  // One link (Settings › Play Together › Invites work): the engine makes the internet invite
  // when that says Anywhere, and the home network's link while the invite service can't be reached.
  const anywhere = netOffered && state.settings.invites !== "wifi";
  const inviting = useRef(false);
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!focusOnOpen?.current) return;
    focusOnOpen.current = false;
    root.current?.focus({ preventScroll: false });
  }, [focusOnOpen]);
  /** Asks the engine for the one link; `renew` voids the old link and makes a new one. */
  const askInvite = async (renew: boolean): Promise<void> => {
    if (inviting.current) return;
    inviting.current = true;
    try {
      const s = anywhere ? await joinApi.inviteShare(addr, renew) : await joinApi.invite(addr, renew);
      setLinkFailed(!s);
      if (s) onState(s);
    } finally {
      inviting.current = false;
    }
  };
  const pickNetwork = (kind: NetworkKind) => {
    setNetwork(kind);
    saveNetwork(kind);
  };

  // Opening the panel opens Play Together with a fresh invite (the engine keeps an open one). While
  // the stream is still starting (a restart at a new bitrate) it asks again (#720).
  useEffect(() => {
    let alive = true;
    void (async () => {
      const reply = await openWhenPlaying(addr, () => alive);
      if (!alive || !reply) return;
      if (!reply.state) {
        setWhy(reply.error);
        setFailed(reply.error !== "old_helper");
        return;
      }
      const opened = reply.state;
      setWhy(null);
      setFailed(false);
      onState(opened);
      // The one link: the internet invite when invites work anywhere (the engine keeps to the
      // home network's when they work only on the Wi-Fi, or it can't make one).
      const net = opened.settings.internetInvites !== null && opened.settings.invites !== "wifi";
      if (net ? !opened.invite?.internet : !opened.invite) {
        const s = net ? await joinApi.inviteShare(addr) : await joinApi.invite(addr);
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
  const netInvite = invite?.internet ?? null;
  // The internet link went missing (a new invite, the engine restarted, the setting turned back
  // to Anywhere): ask for the one link again, once per invite.
  const needNet = anywhere && !!invite && !netInvite && helperOk && !failed;
  useEffect(() => {
    if (needNet) void askInvite(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [needNet, invite?.code]);
  const one = invite ? shareOf(invite) : null;
  // The internet link (works anywhere, the same Wi-Fi included), or the Wi-Fi's own.
  const onNet = one?.works === "anywhere" && !!one.url;
  // Tailscale, when that was the last choice and this computer still has it.
  const lanPick = picked ?? (network === "tailscale" ? splitByNetwork(addresses, invite?.tailscale ?? null).tailnet : null);
  const lanLink = invite ? (lanPick ? inviteOn(invite.url, lanPick) : invite.url) : null;
  const link = onNet ? (one?.url ?? null) : lanLink;
  // While the invite service answers: "Connecting to the invite service…" (muted).
  const netNote = onNet ? netInviteNote(tr, null, netInvite) : null;
  // The Wi-Fi's link although invites work anywhere: no internet here right now (or the invite
  // service is down); or the settings keep invites on the Wi-Fi.
  const wifiWhy = !onNet && netOffered && invite ? (anywhere ? (one?.why === "offline" ? "offline" : null) : "setting") : null;
  const peers = netPeersView(state.netPeers, dismissed);
  const slow = slowFriend(state);
  // A guest whose connection has stayed under the stream's rate: a lower rate that fits everyone,
  // offered once per guest (never applied on its own: every guest shares the PS5's one stream).
  const lower = lowerForAll(state.guests, RP_BITRATES);
  const [lowerSeen, setLowerSeen] = useState<Set<number>>(() => new Set());
  const hasStream = useRemotePlayStore((s) => !!s.stream);
  const lowerOffer = lower && !lowerSeen.has(lower.guest.id) ? lower : null;
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
      ref={root}
      tabIndex={-1}
      aria-labelledby="pt-panel-title"
      role="region"
      className="space-y-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4 outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
      data-testid="pt-panel"
    >
      <div className="flex items-center gap-2">
        <Users size={16} />
        <h3 id="pt-panel-title" className="flex-1 text-sm font-semibold">
          {tr("pt_title", undefined, "Play Together")}
        </h3>
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
      {helperOk && !failed && pictureHere && state && streamSeatFriend(state)?.streamSeat && (
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
            {netNote && (
              <p
                className={`text-xs ${netNote.tone === "warn" ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}
                data-testid={netNote.id}
              >
                {netNote.text}
              </p>
            )}
            {link && onNet && (
              <p className="text-xs text-[var(--color-muted)]" data-testid="pt-works-anywhere">
                {tr("pt_works_anywhere", undefined, "Works anywhere: on your Wi-Fi and over the internet.")}
              </p>
            )}
            {/* #720: Play Together never sets up Tailscale; it only uses the Tailscale app's
                network when both have it and it is the shorter way (join/pathRace, ICE). */}
            {link && onNet && (
              <p className="text-xs text-[var(--color-muted)]" data-testid="pt-no-tailscale">
                {tr(
                  "pt_no_tailscale_needed",
                  undefined,
                  "No Tailscale needed. The link takes the shortest way: your Wi-Fi, Tailscale if you both use the Tailscale app, or the internet.",
                )}
              </p>
            )}
            {link && onNet && <InternetHint testId="pt-addresses" />}
            {link && wifiWhy && (
              <p
                className={`text-xs ${wifiWhy === "offline" ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}
                data-testid="pt-wifi-only"
                data-why={wifiWhy}
              >
                {wifiWhy === "offline"
                  ? tr("pt_wifi_only_offline", undefined, "Works on your Wi-Fi only (no internet right now).")
                  : tr(
                      "pt_wifi_only_setting",
                      undefined,
                      "Invites work only on your Wi-Fi. For friends somewhere else, set Invites work to Anywhere in Settings › App › Play Together.",
                    )}
              </p>
            )}
            {/* The Wi-Fi's link on a computer with more than one network: which one. The one
                link needs no choice (ICE finds the way). */}
            {invite && !onNet && (
              <NetworkChoice
                urls={addresses}
                tailscale={invite.tailscale ?? null}
                current={link}
                onPick={(u) => {
                  setPicked(u);
                  pickNetwork(u && invite.tailscale && urlHost(u) === invite.tailscale ? "tailscale" : "home");
                }}
                otherNetwork={tr("pt_other_network", undefined, "This computer is on more than one network: pick the one your friend is on.")}
                testId="pt-addresses"
                addressTestId="pt-address"
              />
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
            {state.asks.length > 0 && state.seatNext && <SeatNextNote name={friendName(tr, state.asks[0].name)} />}
            {state.asks.length > 0 && !state.seatNext && state.nextProfile && (
              <ProfileNextNote name={friendName(tr, state.asks[0].name)} profile={state.nextProfile.name} />
            )}
          </Step>
          <Step n={3} title={tr("pt_step_tv", undefined, "On the TV")}>
            <TvHint />
            <p className="text-xs text-[var(--color-muted)]">
              {tr(
                "pt_step_tv_body",
                undefined,
                "Nothing to do on your side. Your friend presses PS on their own controller (on their screen) and picks Quick Play or their user on the TV.",
              )}
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

      {helperOk && !failed && peers.connecting.length > 0 && (
        <ul className="space-y-1" data-testid="pt-net-peers">
          {peers.connecting.map((p) => (
            <li key={p.gid} className="flex items-center gap-2 text-xs" data-testid="pt-net-peer">
              <span className="font-medium">{tr("pt_net_friend", undefined, "A friend on the internet")}</span>
              <span className="text-[var(--color-muted)]">{tr("pt_net_connecting", undefined, "Connecting…")}</span>
            </li>
          ))}
        </ul>
      )}
      {helperOk && !failed && peers.failed.length > 0 && (
        <div
          className="flex items-start gap-2 rounded-lg bg-[var(--color-warn-soft)] px-3 py-2 text-xs text-[var(--color-warn)]"
          data-testid="pt-net-peer-failed"
        >
          <span className="flex-1">{tr("pt_net_peer_failed", undefined, "Couldn't connect from their network")}</span>
          <button
            type="button"
            onClick={() => setDismissed((d) => new Set([...d, ...peers.failed.map((p) => p.gid)]))}
            aria-label={tr("rp_phone_close", undefined, "Close")}
            className="rounded p-0.5 hover:bg-[var(--color-surface-3)]"
            data-testid="pt-net-peer-failed-dismiss"
          >
            <X size={14} />
          </button>
        </div>
      )}

      {lowerOffer && (
        <div data-testid="pt-lower" data-to={lowerOffer.toKbps}>
          <Callout
            tone="warn"
            title={tr(
                  "pt_lower_for_all",
                  {
                    name: friendName(tr, lowerOffer.guest.name),
                    got: Math.round(lowerOffer.gotKbps / 100) / 10,
                    to: lowerOffer.toKbps / 1000,
                  },
                  `${friendName(tr, lowerOffer.guest.name)}'s connection carries about ${Math.round(lowerOffer.gotKbps / 100) / 10} Mbps. Lower the stream to ${lowerOffer.toKbps / 1000} Mbps so everyone keeps up? The picture restarts for a few seconds and everyone keeps their place.`,
                )}
          >
              <div className="flex flex-wrap gap-2">
                {hasStream && (
                  <Button
                    size="sm"
                    variant="primary"
                    data-testid="pt-lower-go"
                    onClick={() => {
                      setLowerSeen((s) => new Set([...s, lowerOffer.guest.id]));
                      void useRpQualityStore.getState().restartWith({ bitrateKbps: lowerOffer.toKbps });
                    }}
                  >
                    {tr("pt_lower_to", { to: lowerOffer.toKbps / 1000 }, `Lower to ${lowerOffer.toKbps / 1000} Mbps`)}
                  </Button>
                )}
                <Button size="sm" variant="ghost" data-testid="pt-lower-dismiss" onClick={() => setLowerSeen((s) => new Set([...s, lowerOffer.guest.id]))}>
                  {tr("rp_phone_close", undefined, "Close")}
                </Button>
              </div>
          </Callout>
        </div>
      )}

      {slow && !lowerOffer && (
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
            hint={tr(
              "pt_block_hint",
              undefined,
              "Friends get one press of PS to pick a user when they join; after that PS and Create are off.",
            )}
          />
        </div>
      )}

      {helperOk && !failed && (
        <div data-testid="pt-stream-seat">
          <Toggle
            checked={state.settings.streamSeat}
            onChange={(on) => {
              onState({ ...state, settings: { ...state.settings, streamSeat: on } });
              void joinApi.settings(addr, { stream_seat: on }).then((s) => s && onState(s));
            }}
            label={tr("pt_stream_seat", undefined, "A friend can play as me on Player 1 (when this device shows no picture)")}
            hint={tr("pt_stream_seat_hint", undefined, "Off: friends always get their own controller and pick a user on the TV.")}
          />
        </div>
      )}

      {/* An engine from before internet invites: Tailscale is the way for a friend elsewhere. */}
      {!netOffered && (
        <p className="flex items-start gap-2 text-xs text-[var(--color-muted)]" data-testid="pt-tailscale">
          <Gamepad2 size={14} className="mt-0.5 shrink-0" />
          {tr(
            "pt_tailscale",
            undefined,
            "Playing from somewhere else? Both of you join the same Tailscale network, then share the link.",
          )}
        </p>
      )}

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
export function JoinAsks({
  addr,
  asks,
  seatNext = false,
  nextProfile = null,
}: {
  addr: string | null;
  asks: JoinHostState["asks"];
  /** Allowing the first one gives them this stream's controller (Player 1, as the host's user). */
  seatNext?: boolean;
  /** Allowing the first one seats them in this PS5 profile (the host's choice for their player). */
  nextProfile?: string | null;
}) {
  const tr = useTr();
  if (!asks.length || !addr) return null;
  const a = asks[0];
  return (
    <div
      className={`pointer-events-auto absolute left-1/2 top-28 z-40 flex max-w-[min(32rem,calc(100%-2rem))] -translate-x-1/2 flex-wrap items-center gap-2 bg-black/85 px-4 py-2 text-sm text-white shadow-lg ${
        seatNext || nextProfile ? "rounded-2xl" : "rounded-full"
      }`}
      data-testid="pt-ask"
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <Users size={14} />
      <span>{tr("pt_asks", { name: friendName(tr, a.name) }, `${friendName(tr, a.name)} wants to play with you`)}</span>
      {seatNext && <SeatNextNote name={friendName(tr, a.name)} className="order-last w-full text-xs text-white/75" />}
      {!seatNext && nextProfile && (
        <ProfileNextNote name={friendName(tr, a.name)} profile={nextProfile} className="order-last w-full text-xs text-white/75" />
      )}
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
