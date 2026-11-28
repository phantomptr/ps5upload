import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { MessageCircle, Mic, MicOff, Users, X } from "lucide-react";

import { PingBars } from "../components/PingBars";
import { RoomChatBox } from "../components/RoomChatBox";
import { quickText, unread, hideRude } from "../lib/room/chat";
import { RoomVoice, micAvailability } from "../lib/room/roomVoice";
import { DEFAULT_PTT_KEY, voiceForTier } from "../lib/room/voice";
import { useTr } from "../state/lang";
import { readTier } from "./routeTier";
import { commsRefusedText, useRoomLink, type RoomLink } from "./roomLink";

/**
 * The room over a guest's game (design 7.2, 7.5, 7A): a strip top-right with their own ping, the
 * host's PS5 link, chat (with an unread count) and the mic; the last three chat lines bottom-left
 * (fading after 6 s, above any touch controls); a drawer with the seats, the chat, per-person
 * volume and voice numbers. Nothing shows for a host without rooms.
 */
export function RoomOverlay({
  link,
  ownRttMs,
  ownTier,
  ensureNotice,
  onRules,
  touch = false,
}: {
  link: RoomLink;
  /** This page's own reading, when the room push hasn't said yet. */
  ownRttMs: number | null;
  ownTier: string | null;
  /** Shows the responsibility notice when this device hasn't accepted it (true once accepted). */
  ensureNotice: (what: "chat" | "voice") => Promise<boolean>;
  onRules: () => void;
  /** Touch controls are on screen: keep the chat lines above them. */
  touch?: boolean;
}) {
  const tr = useTr();
  const snap = useRoomLink(link);
  const room = snap.room;
  const [open, setOpen] = useState(false);
  // People hidden on this device (by name): their voice muted, their chat lines not shown here.
  const [hidden, setHidden] = useState<ReadonlySet<string>>(() => new Set());
  const [told, setTold] = useState<ReadonlySet<number>>(() => new Set());
  const voice = useMemo(() => new RoomVoice({ sendBinary: (b) => link.sendBinary(b), mic: (m) => link.mic(m) }), [link]);
  const micState = useSyncExternalStore(voice.subscribe, voice.getState, voice.getState);
  // Held / how / volumes: redraw on every change.
  useSyncExternalStore(voice.subscribe, voice.getVersion, voice.getVersion);
  useEffect(() => {
    link.onVoice = (d) => voice.onBundle(d);
    return () => {
      link.onVoice = null;
      voice.close();
    };
  }, [link, voice]);
  // The room's rules for talking, and this connection's tier (Poor pauses voice).
  const youTier = readTier(room?.you?.tier ?? ownTier);
  useEffect(() => {
    voice.setTalk({
      room: snap.voiceMode ?? room?.voice ?? "off",
      hostMuted: snap.muted.voice,
      paused: snap.voicePaused || snap.kbps === 0 || voiceForTier(youTier).paused,
    });
    if (snap.kbps > 0) voice.setRate(snap.kbps, snap.frameMs);
  }, [voice, snap.voiceMode, room?.voice, snap.muted.voice, snap.voicePaused, youTier, snap.kbps, snap.frameMs]);
  // The notice on this device, then the engine told (it sends chat and voice only after).
  const notice = async (what: "chat" | "voice"): Promise<boolean> => {
    const ok = await ensureNotice(what);
    if (ok && (!snap.comms?.accepted || snap.needNotice)) link.acceptNotice();
    return ok;
  };
  // Push-to-talk on the backquote key (never a key the game uses: the guest keymap has none).
  useEffect(() => {
    if (micState !== "live") return;
    const down = (e: KeyboardEvent) => {
      if (e.code === DEFAULT_PTT_KEY && !e.repeat) voice.hold(true);
    };
    const up = (e: KeyboardEvent) => {
      if (e.code === DEFAULT_PTT_KEY) voice.hold(false);
    };
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
    };
  }, [voice, micState]);
  const [now, setNow] = useState(0);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  useEffect(() => {
    if (open) link.markSeen();
  }, [open, snap.lines.length, link]);
  if (!room && !snap.comms) return null;
  const chatMode = snap.comms?.chat ?? room?.chat ?? "off";
  const chatOn = chatMode !== "off";
  const caps = snap.comms?.caps ?? room?.caps ?? [];
  const voiceOn = (snap.voiceMode ?? room?.voice ?? "off") !== "off" && caps.includes("voice");
  const refusedLine = snap.refused && now - snap.refused.at < 4000 ? commsRefusedText(tr, snap.refused) : null;
  const seats = room?.players.length
    ? room.players.map((p, i) => ({ key: `${p.seat ?? i}-${p.name}`, name: p.name, seat: p.seat, you: p.you, color: p.color, sid: snap.people.find((x) => x.name === p.name)?.sid ?? null }))
    : snap.people.map((p) => ({ key: `s${p.sid}`, name: p.name, seat: p.player, you: p.sid === snap.you, color: null as [number, number, number] | null, sid: p.sid }));
  const unreadN = unread(snap.lines, snap.seen);
  const recent = snap.lines.filter((l) => now - l.at < 6000).slice(-3);
  const publicRoom = room?.visibility === "public";
  const avail = micAvailability();
  const ptt = voice.talk.room === "ptt" || voice.talk.how === "ptt";
  const toggleMic = async () => {
    if (micState === "live") return voice.stop();
    if (!(await notice("voice"))) return;
    await voice.start();
  };
  const chip = "pointer-events-auto flex items-center gap-1.5 rounded-full bg-black/60 px-2.5 py-1 text-xs text-white";
  return (
    <>
      <div className="pointer-events-none absolute end-2 top-2 z-20 flex flex-wrap items-center justify-end gap-1.5" data-testid="room-strip">
        <span className={chip} data-testid="room-strip-you">
          {tr("room_you", undefined, "You")}
          <PingBars compact tier={youTier} ms={room?.you?.rttMs ?? ownRttMs} testId="room-strip-you-bars" />
        </span>
        {room?.hostLink && (
          <span className={chip} data-testid="room-strip-host">
            {tr("rooms_host_ps5", undefined, "Host's PS5")}
            <PingBars compact tier={readTier(room.hostLink.tier)} ms={room.hostLink.rttMs} testId="room-strip-host-bars" />
          </span>
        )}
        {(chatOn || voiceOn) && (
          <button type="button" className={chip} onClick={() => setOpen(true)} aria-label={tr("room_chat", undefined, "Chat")} data-testid="room-strip-chat">
            <MessageCircle size={14} />
            {unreadN > 0 && (
              <span className="rounded-full bg-red-500 px-1.5 text-[10px] font-bold" data-testid="room-strip-unread">
                {unreadN}
              </span>
            )}
          </button>
        )}
        {voiceOn && (
          <button
            type="button"
            className={`${chip} ${micState === "live" && (!ptt || voice.talk.held) ? "ring-2 ring-green-400" : ""}`}
            onClick={() => void toggleMic()}
            aria-pressed={micState === "live"}
            aria-label={micState === "live" ? tr("room_mic_off", undefined, "Mic off") : tr("room_mic_on", undefined, "Mic on")}
            data-testid="room-strip-mic"
            data-state={micState}
          >
            {micState === "live" ? <Mic size={14} /> : <MicOff size={14} />}
          </button>
        )}
        {voiceOn && micState === "live" && ptt && (
          <button
            type="button"
            className={`${chip} select-none ${voice.talk.held ? "bg-green-600" : ""}`}
            onPointerDown={(e) => {
              e.stopPropagation();
              voice.hold(true);
            }}
            onPointerUp={() => voice.hold(false)}
            onPointerLeave={() => voice.hold(false)}
            data-testid="room-strip-ptt"
          >
            {tr("room_hold_to_talk", undefined, "Hold to talk")}
          </button>
        )}
      </div>
      {snap.readyUntil != null && now < snap.readyUntil && !snap.saidReady && (
        <div className="pointer-events-auto absolute left-1/2 top-12 z-30 -translate-x-1/2 rounded-full bg-black/80 px-4 py-2 text-sm text-white" data-testid="room-ready">
          <button type="button" className="font-semibold" onClick={() => link.ready()} data-testid="room-ready-yes">
            {tr("room_ready_q", undefined, "Ready?")} ✓
          </button>
        </div>
      )}
      {refusedLine && !open && (
        <p className="pointer-events-none absolute left-1/2 top-12 z-30 -translate-x-1/2 rounded-full bg-black/80 px-3 py-1 text-xs text-white" data-testid="room-refused">
          {refusedLine}
        </p>
      )}
      {!open && recent.length > 0 && (
        <ul className={`pointer-events-none absolute start-2 z-20 max-w-[70%] space-y-0.5 text-xs text-white ${touch ? "bottom-40" : "bottom-14"}`} data-testid="room-chat-recent">
          {recent.filter((l) => !hidden.has(l.from)).map((l) => (
            <li key={l.n} className="w-fit rounded bg-black/55 px-2 py-0.5 [overflow-wrap:anywhere]">
              <b>{l.mine ? tr("room_chat_you", undefined, "You") : l.from}:</b>{" "}
              {l.quick != null ? quickText(tr, l.quick) : publicRoom ? hideRude(l.text ?? "") : l.text}
            </li>
          ))}
        </ul>
      )}
      {open && (
        <div
          className="pointer-events-auto absolute inset-y-0 end-0 z-40 w-full max-w-sm space-y-3 overflow-y-auto bg-neutral-950/95 p-4 text-white"
          data-testid="room-drawer"
          onPointerDown={(e) => e.stopPropagation()}
        >
          <div className="flex items-center gap-2">
            <Users size={16} />
            <h3 className="min-w-0 flex-1 truncate text-sm font-semibold">{room?.name ?? tr("room_title", undefined, "Play Room")}</h3>
            <button type="button" onClick={() => setOpen(false)} aria-label={tr("rp_phone_close", undefined, "Close")} className="rounded p-1 hover:bg-white/10">
              <X size={16} />
            </button>
          </div>
          <ul className="space-y-1 text-sm" data-testid="room-drawer-seats">
            {seats.map((p) => {
              const id = p.sid ?? -1;
              const speaking = p.sid != null && snap.speaking.includes(p.sid);
              const pv = voice.person(id);
              return (
                <li key={p.key} className="flex items-center gap-2">
                  <span className="inline-block h-2.5 w-2.5 rounded-full" style={{ background: p.color ? `rgb(${p.color.join(",")})` : "#888" }} />
                  <span className={`min-w-0 flex-1 truncate ${speaking ? "rounded px-1 ring-2 ring-green-400" : ""}`}>
                    {p.seat ? `${p.seat} · ` : ""}
                    {p.name}
                    {p.you ? ` (${tr("room_you", undefined, "You")})` : ""}
                  </span>
                  {voiceOn && !p.you && p.sid != null && (
                    <>
                      <input
                        type="range"
                        min={0}
                        max={200}
                        step={10}
                        value={pv.pct}
                        onChange={(e) => voice.setPerson(id, { pct: Number(e.target.value) })}
                        aria-label={tr("room_volume_of", { name: p.name }, `${p.name}'s volume`)}
                        className="w-20"
                      />
                      <button type="button" className="text-xs underline" onClick={() => voice.setPerson(id, { muted: !pv.muted })}>
                        {pv.muted ? tr("room_unmute", undefined, "Unmute") : tr("room_mute", undefined, "Mute")}
                      </button>
                    </>
                  )}
                  {!p.you && (
                    <button
                      type="button"
                      className="text-xs underline"
                      onClick={() => {
                        const on = !hidden.has(p.name);
                        const next = new Set(hidden);
                        if (on) next.add(p.name);
                        else next.delete(p.name);
                        setHidden(next);
                        if (p.sid != null) voice.setPerson(p.sid, { muted: on });
                      }}
                      data-testid="room-hide-person"
                    >
                      {hidden.has(p.name) ? tr("room_show_person", undefined, "Show") : tr("room_hide_person", undefined, "Hide")}
                    </button>
                  )}
                  {!p.you && p.sid != null && p.sid !== 0 && (
                    <button
                      type="button"
                      className="text-xs underline disabled:no-underline disabled:opacity-50"
                      disabled={told.has(p.sid)}
                      onClick={() => {
                        if (p.sid != null && link.flag(p.sid)) setTold(new Set([...told, p.sid]));
                      }}
                      data-testid="room-tell-host"
                    >
                      {told.has(p.sid) ? tr("room_told_host", undefined, "Host told") : tr("room_tell_host", undefined, "Tell the host")}
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
          {voiceOn && (
            <div className="space-y-1 text-xs text-white/70" data-testid="room-drawer-voice">
              {avail === "insecure" && <p data-testid="room-voice-insecure">{tr("room_voice_insecure", undefined, "To talk too, open the secure invite link.")}</p>}
              {micState === "denied" && <p>{tr("room_voice_denied", undefined, "The microphone is blocked for this page. Allow it in the browser's settings.")}</p>}
              {snap.muted.voice && <p>{tr("room_voice_host_muted", undefined, "The host muted your mic")}</p>}
              {voice.talk.paused && <p>{tr("room_voice_paused", undefined, "Voice paused: your connection needs it for the game")}</p>}
              {micState === "live" && (
                <p>
                  {ptt
                    ? tr("room_ptt_hint", undefined, "Push-to-talk: hold ` or the Hold to talk button.")
                    : tr("room_open_mic_hint", undefined, "Open mic. Use headphones so others don't hear the game twice.")}
                </p>
              )}
              {micState === "live" && voice.talk.room === "open" && (
                <button type="button" className="underline" onClick={() => voice.setTalk({ how: voice.talk.how === "ptt" ? "open" : "ptt" })} data-testid="room-voice-how">
                  {voice.talk.how === "ptt" ? tr("room_use_open_mic", undefined, "Use open mic") : tr("room_use_ptt", undefined, "Use push-to-talk")}
                </button>
              )}
              <VoiceStats voice={voice} />
            </div>
          )}
          {refusedLine && <p className="text-xs text-amber-300" data-testid="room-drawer-refused">{refusedLine}</p>}
          {chatOn && (
            <RoomChatBox
              dark
              lines={snap.lines}
              mode={chatMode}
              mutedByHost={snap.muted.chat}
              publicRoom={publicRoom}
              rude={publicRoom}
              ensureNotice={() => notice("chat")}
              onSend={(m) => void link.chat(m)}
              hidden={hidden}
            />
          )}
          <button type="button" className="text-xs text-white/60 underline" onClick={onRules} data-testid="room-rules">
            {tr("room_notice_link", undefined, "Rules & responsibility")}
          </button>
        </div>
      )}
    </>
  );
}

/** "Voice 24 kb/s in · 12 out" for the stats (design 7A.1b, Measured). */
function VoiceStats({ voice }: { voice: RoomVoice }) {
  const tr = useTr();
  const [m, setM] = useState({ inKbps: 0, outKbps: 0 });
  useEffect(() => {
    const t = setInterval(() => setM(voice.meter.read()), 1000);
    return () => clearInterval(t);
  }, [voice]);
  return (
    <p className="tabular-nums" data-testid="room-voice-stats">
      {tr("room_voice_stats", { in: m.inKbps, out: m.outKbps }, `Voice ${m.inKbps} kb/s in · ${m.outKbps} out`)}
    </p>
  );
}
