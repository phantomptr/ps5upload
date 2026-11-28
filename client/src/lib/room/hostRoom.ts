// The host's own side of the room's chat and voice (API.md "Play Rooms chat and voice"): it rides
// the host page's Remote Play socket (`/api/ps5/rp/ws`), where the mic switch is `room_mic` (`mic`
// there is the console's own microphone). RemotePlayStream hands this link what the engine sends
// (comms, voice, chat, refusals, 0x0B bundles) and lets it send on its socket.

import { readServerMessage } from "../joinProtocol";
import { RoomLink } from "../../join/roomLink";
import { setRoomTap } from "../remotePlayStream";

export const hostRoom = new RoomLink();
hostRoom.micKey = "room_mic";

const ROOM_KEYS = ["comms", "voice", "chat", "chat_history", "comms_refused", "muted", "flagged"];

/** A text message from the Remote Play socket that is the room's: handled here (true), else left
 *  to the stream. */
export function hostRoomText(text: string): boolean {
  if (!ROOM_KEYS.some((k) => text.includes(`"${k}"`))) return false;
  let v: Record<string, unknown>;
  try {
    v = JSON.parse(text) as Record<string, unknown>;
  } catch {
    return false;
  }
  if (!v || typeof v !== "object" || !ROOM_KEYS.some((k) => k in v)) return false;
  if ("flagged" in v) return true;
  const m = readServerMessage(text);
  if (!m) return false;
  switch (m.kind) {
    case "comms":
      hostRoom.onComms(m.comms);
      return true;
    case "voice":
      hostRoom.onVoiceState(m.voice);
      return true;
    case "chat":
      hostRoom.onChat(m.line);
      return true;
    case "chat_history":
      hostRoom.onChatHistory(m.lines);
      return true;
    case "comms_refused":
      hostRoom.onRefused(m.refused);
      return true;
    case "muted":
      hostRoom.onMuted({ voice: m.voice, chat: m.chat });
      return true;
    default:
      return false;
  }
}

/** A binary message from the Remote Play socket: a voice bundle (0x0B) is the room's. */
export function hostRoomBinary(data: ArrayBuffer): boolean {
  if (data.byteLength < 2 || new Uint8Array(data, 0, 1)[0] !== 0x0b) return false;
  hostRoom.onVoice?.(data);
  return true;
}

/** The Remote Play socket the host's room talks on (null: none open). */
export function attachHostRoom(ws: WebSocket | null): void {
  if (!ws) {
    hostRoom.sendJson = () => false;
    hostRoom.sendBinary = () => false;
    return;
  }
  hostRoom.sendJson = (m) => {
    if (ws.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify(m));
    return true;
  };
  hostRoom.sendBinary = (b) => {
    if (ws.readyState !== WebSocket.OPEN || ws.bufferedAmount > 64 * 1024) return false;
    ws.send(b);
    return true;
  };
}

// On the stream's socket from now on (the app's room panel imports this module).
setRoomTap({ text: hostRoomText, binary: hostRoomBinary, attach: attachHostRoom });
