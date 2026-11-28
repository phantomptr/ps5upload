// Play Rooms, the guest's side of one session (design 7.5, 7A; API.md "Play Rooms chat and
// voice"): the room push (seats, the guest's own ping, the host's PS5 link), chat and voice state
// from the host engine, host mutes, the ready check, and what the guest sends. Used by the guest
// page and the app's guest player alike. Old hosts send none of it, and nothing then shows.

import { useSyncExternalStore } from "react";

import { cleanChat, type ChatLine } from "../lib/room/chat";
import { NOTICE_VERSION, noticeAccepted } from "../lib/room/notice";
import type { CommsMsg, CommsRefused, EngineChatLine, GuestRoom, VoicePerson, VoiceStateMsg } from "../lib/joinProtocol";

export interface RoomSnap {
  room: GuestRoom | null;
  comms: CommsMsg | null;
  lines: ChatLine[];
  /** The last line number seen (the chat open or read). */
  seen: number;
  speaking: number[];
  /** This person's speaker id, and everyone the voice state names. */
  you: number | null;
  people: VoicePerson[];
  /** Voice paused for this guest: their connection needs it for the game. */
  voicePaused: boolean;
  voiceMode: "off" | "ptt" | "open" | null;
  /** What to encode at (the engine's say): 24 or 12 kb/s, 20 or 40 ms; 0 paused. */
  kbps: number;
  frameMs: number;
  muted: { voice: boolean; chat: boolean };
  /** The last refusal, for a short line ("Slow down", "The host muted chat for you"). */
  refused: (CommsRefused & { at: number }) | null;
  /** The engine wants the notice accepted first (its refusal said so). */
  needNotice: boolean;
  /** A ready check runs until this time (ms, this clock); null: none. */
  readyUntil: number | null;
  saidReady: boolean;
}

const EMPTY: RoomSnap = {
  room: null,
  comms: null,
  lines: [],
  seen: 0,
  speaking: [],
  you: null,
  people: [],
  voicePaused: false,
  voiceMode: null,
  kbps: 24,
  frameMs: 20,
  muted: { voice: false, chat: false },
  refused: null,
  needNotice: false,
  readyUntil: null,
  saidReady: false,
};

/** The engine's lines as this device shows them: folded repeats replace their line (`x`), the
 *  sender's own (same speaker id) marked as theirs; at most 50. */
export function mergeLines(lines: readonly ChatLine[], add: readonly EngineChatLine[], you: number | null, at: number): ChatLine[] {
  let out = [...lines];
  for (const l of add) {
    const line: ChatLine = {
      n: l.id,
      from: l.from,
      mine: you != null && l.sid === you,
      quick: l.quick,
      text: l.text ? cleanChat(l.text) : null,
      times: l.x,
      at,
    };
    const i = out.findIndex((x) => x.n === l.id);
    if (i >= 0) out[i] = line;
    else out.push(line);
  }
  out = out.sort((a, b) => a.n - b.n).slice(-50);
  return out;
}

export class RoomLink {
  private s: RoomSnap = EMPTY;
  private listeners = new Set<() => void>();
  /** Sends one JSON message to the host engine (false: not joined). */
  sendJson: (m: Record<string, unknown>) => boolean = () => false;
  /** Sends one binary message (voice). */
  sendBinary: (b: Uint8Array<ArrayBuffer>) => boolean = () => false;
  /** Voice bundles in (0x0B), for the player (roomVoice). */
  onVoice: ((data: ArrayBuffer) => void) | null = null;
  /** The key the mic switch goes under: `mic` on a guest socket, `room_mic` on the host page's. */
  micKey = "mic";

  constructor(private readonly now: () => number = () => Date.now()) {}

  readonly subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  readonly snap = (): RoomSnap => this.s;

  private set(p: Partial<RoomSnap>): void {
    this.s = { ...this.s, ...p };
    for (const l of this.listeners) l();
  }

  onRoom(room: GuestRoom): void {
    this.set({ room, muted: room.muted, voiceMode: room.voice ?? this.s.voiceMode });
  }

  onComms(c: CommsMsg): void {
    this.set({ comms: c, voiceMode: c.voice ?? this.s.voiceMode });
    // Accepted on this device already: tell the engine (it asks once per connection).
    if (!c.accepted && noticeAccepted()) this.acceptNotice();
  }

  /** The person accepted the notice: the engine starts sending chat and voice. */
  acceptNotice(): void {
    this.sendJson({ notice: NOTICE_VERSION });
    this.set({ needNotice: false });
  }

  onChat(l: EngineChatLine): void {
    this.set({ lines: mergeLines(this.s.lines, [l], this.s.you, this.now()) });
  }

  onChatHistory(lines: EngineChatLine[]): void {
    this.set({ lines: mergeLines([], lines, this.s.you, this.now()) });
    this.markSeen();
  }

  onVoiceState(v: VoiceStateMsg): void {
    if (!v.full) {
      this.set({ speaking: v.speaking });
      return;
    }
    this.set({
      speaking: v.speaking,
      you: v.you,
      people: v.people,
      voicePaused: v.paused === "poor",
      voiceMode: v.mode ?? this.s.voiceMode,
      kbps: v.kbps,
      frameMs: v.frameMs,
    });
  }

  onMuted(m: { voice: boolean; chat: boolean }): void {
    this.set({ muted: m });
  }

  onRefused(r: CommsRefused): void {
    this.set({ refused: { ...r, at: this.now() }, needNotice: r.why === "notice" || this.s.needNotice });
  }

  onReady(leftS: number): void {
    this.set({ readyUntil: this.now() + leftS * 1000, saidReady: false });
  }

  /** The chat was looked at: nothing unread. */
  markSeen(): void {
    const last = this.s.lines[this.s.lines.length - 1]?.n ?? 0;
    if (last !== this.s.seen) this.set({ seen: last });
  }

  /** A chat line from this person; it shows when the engine sends it back to everyone. */
  chat(m: { quick: number } | { text: string }): boolean {
    return this.sendJson({ chat: m });
  }

  /** The mic: on, off, or push-to-talk held / let go. */
  mic(m: "on" | "off" | "ptt_down" | "ptt_up"): boolean {
    return this.sendJson({ [this.micKey]: m });
  }

  ready(): boolean {
    const ok = this.sendJson({ ready: true });
    if (ok) this.set({ saidReady: true });
    return ok;
  }

  /** "Tell the host" about a person (by speaker id), once per person per room. */
  private told = new Set<number>();
  flag(sid: number): boolean {
    if (this.told.has(sid)) return false;
    const ok = this.sendJson({ flag: sid });
    if (ok) this.told.add(sid);
    return ok;
  }

  reset(): void {
    this.s = EMPTY;
    for (const l of this.listeners) l();
  }
}

export function useRoomLink(link: RoomLink): RoomSnap {
  return useSyncExternalStore(link.subscribe, link.snap, link.snap);
}

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** A chat or voice refusal in plain words (API.md `comms_refused.why`). */
export function commsRefusedText(tr: Tr, r: Pick<CommsRefused, "why" | "retryMs">): string {
  switch (r.why) {
    case "notice":
      return tr("room_refused_notice", undefined, "Accept Rules & responsibility first.");
    case "off":
      return tr("room_chat_is_off", undefined, "Chat is off");
    case "quick_only":
      return tr("room_refused_quick_only", undefined, "This room takes quick messages only.");
    case "muted":
      return tr("room_refused_muted", undefined, "The host muted you.");
    case "slow_down": {
      const s = Math.max(1, Math.ceil((r.retryMs ?? 1000) / 1000));
      return tr("room_refused_slow", { s }, `Slow down. Try again in ${s} s.`);
    }
    case "empty":
      return tr("room_refused_empty", undefined, "Type something first.");
    case "too_long":
      return tr("room_refused_too_long", undefined, "That's too long: 140 characters at most.");
    case "bad_quick":
      return tr("room_refused_bad_quick", undefined, "The host's PS5Upload doesn't know that quick message.");
    case "ptt_only":
      return tr("room_refused_ptt_only", undefined, "This room is push-to-talk only: hold the talk key.");
    case "mic_off":
      return tr("room_refused_mic_off", undefined, "Turn your mic on first.");
    case "poor":
      return tr("room_voice_paused", undefined, "Voice paused: your connection needs it for the game");
    case "not_open":
      return tr("room_err_not_open", undefined, "Your room isn't open.");
    default:
      return tr("room_err_engine", undefined, "That didn't work. Try again.");
  }
}
