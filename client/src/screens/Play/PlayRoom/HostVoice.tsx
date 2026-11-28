import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { Mic, MicOff } from "lucide-react";

import { Button } from "../../../components";
import { commsRefusedText, useRoomLink } from "../../../join/roomLink";
import { hostRoom } from "../../../lib/room/hostRoom";
import { RoomVoice } from "../../../lib/room/roomVoice";
import { DEFAULT_PTT_KEY } from "../../../lib/room/voice";
import { useTr } from "../../../state/lang";

/**
 * The host's own voice in their room (design 7A.5, desktop/phone host): the mic (off until turned
 * on), push-to-talk on the backquote key or the Hold to talk button, who is talking, each person's
 * volume here, and the voice numbers. It rides this page's Remote Play socket (lib/room/hostRoom).
 */
export function HostVoice({ ensureNotice, voiceMode }: { ensureNotice: () => Promise<boolean>; voiceMode: "off" | "ptt" | "open" }) {
  const tr = useTr();
  const snap = useRoomLink(hostRoom);
  const voice = useMemo(() => new RoomVoice({ sendBinary: (b) => hostRoom.sendBinary(b), mic: (m) => hostRoom.mic(m) }), []);
  const mic = useSyncExternalStore(voice.subscribe, voice.getState, voice.getState);
  useSyncExternalStore(voice.subscribe, voice.getVersion, voice.getVersion);
  useEffect(() => {
    hostRoom.onVoice = (d) => voice.onBundle(d);
    return () => {
      hostRoom.onVoice = null;
      voice.close();
    };
  }, [voice]);
  useEffect(() => {
    voice.setTalk({ room: snap.voiceMode ?? voiceMode, hostMuted: false, paused: snap.voicePaused || snap.kbps === 0 });
    if (snap.kbps > 0) voice.setRate(snap.kbps, snap.frameMs);
  }, [voice, snap.voiceMode, voiceMode, snap.voicePaused, snap.kbps, snap.frameMs]);
  useEffect(() => {
    if (mic !== "live") return;
    const down = (e: KeyboardEvent) => e.code === DEFAULT_PTT_KEY && !e.repeat && voice.hold(true);
    const up = (e: KeyboardEvent) => e.code === DEFAULT_PTT_KEY && voice.hold(false);
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
    };
  }, [voice, mic]);
  const [stats, setStats] = useState({ inKbps: 0, outKbps: 0 });
  useEffect(() => {
    const t = setInterval(() => setStats(voice.meter.read()), 1000);
    return () => clearInterval(t);
  }, [voice]);
  if (voiceMode === "off") return null;
  const ptt = voice.talk.room === "ptt" || voice.talk.how === "ptt";
  const toggle = async () => {
    if (mic === "live") return voice.stop();
    if (!(await ensureNotice())) return;
    hostRoom.acceptNotice();
    await voice.start();
  };
  const refused = snap.refused ? commsRefusedText(tr, snap.refused) : null;
  const others = snap.people.filter((p) => p.sid !== snap.you);
  return (
    <section className="space-y-2" data-testid="room-host-voice" data-state={mic}>
      <div className="flex flex-wrap items-center gap-2">
        <Button size="sm" variant={mic === "live" ? "primary" : "secondary"} leftIcon={mic === "live" ? <Mic size={14} /> : <MicOff size={14} />} onClick={() => void toggle()} data-testid="room-host-mic">
          {mic === "live" ? tr("room_mic_off", undefined, "Mic off") : tr("room_mic_on", undefined, "Mic on")}
        </Button>
        {mic === "live" && ptt && (
          <button
            type="button"
            className={`select-none rounded-full border border-[var(--color-border)] px-3 py-1 text-xs ${voice.talk.held ? "bg-[var(--color-good)] text-white" : ""}`}
            onPointerDown={() => voice.hold(true)}
            onPointerUp={() => voice.hold(false)}
            onPointerLeave={() => voice.hold(false)}
            data-testid="room-host-ptt"
          >
            {tr("room_hold_to_talk", undefined, "Hold to talk")}
          </button>
        )}
        <span className="text-xs tabular-nums text-[var(--color-muted)]" data-testid="room-host-voice-stats">
          {tr("room_voice_stats", { in: stats.inKbps, out: stats.outKbps }, `Voice ${stats.inKbps} kb/s in · ${stats.outKbps} out`)}
        </span>
      </div>
      {mic === "live" && (
        <p className="text-xs text-[var(--color-muted)]">
          {ptt
            ? tr("room_ptt_hint", undefined, "Push-to-talk: hold ` or the Hold to talk button.")
            : tr("room_open_mic_hint", undefined, "Open mic. Use headphones so others don't hear the game twice.")}
        </p>
      )}
      {mic === "denied" && <p className="text-xs text-[var(--color-warn)]">{tr("room_voice_denied", undefined, "The microphone is blocked for this page. Allow it in the browser's settings.")}</p>}
      {mic === "insecure" && <p className="text-xs text-[var(--color-warn)]">{tr("room_voice_insecure", undefined, "To talk too, open the secure invite link.")}</p>}
      {refused && <p className="text-xs text-[var(--color-warn)]">{refused}</p>}
      {others.length > 0 && (
        <ul className="space-y-1 text-sm">
          {others.map((p) => {
            const pv = voice.person(p.sid);
            return (
              <li key={p.sid} className="flex items-center gap-2">
                <span className={`min-w-0 flex-1 truncate ${snap.speaking.includes(p.sid) ? "rounded px-1 ring-2 ring-[var(--color-good)]" : ""}`}>{p.name}</span>
                <input
                  type="range"
                  min={0}
                  max={200}
                  step={10}
                  value={pv.pct}
                  onChange={(e) => voice.setPerson(p.sid, { pct: Number(e.target.value) })}
                  aria-label={tr("room_volume_of", { name: p.name }, `${p.name}'s volume`)}
                  className="w-24"
                />
                <button type="button" className="text-xs underline" onClick={() => voice.setPerson(p.sid, { muted: !pv.muted })}>
                  {pv.muted ? tr("room_unmute", undefined, "Unmute") : tr("room_mute", undefined, "Mute")}
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
