import { useEffect, useRef, useState } from "react";
import { MessageCircle, Send } from "lucide-react";

import { ChatPace, cleanChat, hideRude, MAX_CHAT, QUICK_IDS, quickText, type ChatMode } from "../lib/room/chat";
import { useTr } from "../state/lang";

/** One line as the box shows it. */
export interface ChatBoxLine {
  n: number;
  from: string;
  mine: boolean;
  quick: number | null;
  text: string | null;
  times: number;
}

/**
 * The room's chat (design 7A.4): quick messages (shown in each person's own language), typed
 * text when the room allows it, never clickable links. Used by the host's room panel, the
 * desktop/phone guest and the guest web page (`dark`). `ensureNotice` asks the responsibility
 * notice before the first message on this device.
 */
export function RoomChatBox({
  lines,
  mode,
  mutedByHost = false,
  publicRoom = false,
  rude = false,
  onSend,
  ensureNotice,
  dark = false,
  hidden = new Set<string>(),
  testId = "room-chat",
}: {
  lines: readonly ChatBoxLine[];
  mode: ChatMode;
  mutedByHost?: boolean;
  publicRoom?: boolean;
  /** Hide rude words (local; on by default in Public). */
  rude?: boolean;
  onSend: (m: { quick: number } | { text: string }) => void | Promise<unknown>;
  ensureNotice: () => Promise<boolean>;
  dark?: boolean;
  /** People hidden on this device (by name): their lines aren't shown. */
  hidden?: ReadonlySet<string>;
  testId?: string;
}) {
  const tr = useTr();
  const [text, setText] = useState("");
  const [slow, setSlow] = useState(false);
  const pace = useRef(new ChatPace());
  const list = useRef<HTMLUListElement>(null);
  useEffect(() => {
    const el = list.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines.length]);
  const send = async (m: { quick: number } | { text: string }) => {
    if (!(await ensureNotice())) return;
    if (!pace.current.take()) {
      setSlow(true);
      window.setTimeout(() => setSlow(false), 2000);
      return;
    }
    setSlow(false);
    await onSend(m);
  };
  const muted = dark ? "text-white/60" : "text-[var(--color-muted)]";
  const chip = dark
    ? "rounded-full bg-white/15 px-2.5 py-1 text-xs hover:bg-white/25"
    : "rounded-full border border-[var(--color-border)] px-2.5 py-1 text-xs hover:bg-[var(--color-surface-3)]";
  const shown = lines.filter((l) => !hidden.has(l.from));
  return (
    <section className="space-y-2" data-testid={testId} data-mode={mode}>
      <h4 className="flex items-center gap-1.5 text-sm font-semibold">
        <MessageCircle size={14} aria-hidden />
        {tr("room_chat", undefined, "Chat")}
      </h4>
      <ul ref={list} className="max-h-40 space-y-0.5 overflow-y-auto text-sm" aria-live="polite" data-testid={`${testId}-lines`}>
        {shown.length === 0 && <li className={`text-xs ${muted}`}>{tr("room_chat_empty", undefined, "No messages yet.")}</li>}
        {shown.map((l) => {
          const body = l.quick != null ? (quickText(tr, l.quick) ?? "…") : (l.text ?? "");
          return (
            <li key={l.n} className="[overflow-wrap:anywhere]" data-testid={`${testId}-line`}>
              <span className="font-semibold">{l.mine ? tr("room_chat_you", undefined, "You") : l.from || tr("pt_friend_fallback", undefined, "Friend")}:</span>{" "}
              <span>{rude ? hideRude(body) : body}</span>
              {l.times > 1 && <span className={`ms-1 text-xs ${muted}`}>×{l.times}</span>}
            </li>
          );
        })}
      </ul>
      {mode === "off" ? (
        <p className={`text-xs ${muted}`} data-testid={`${testId}-off`}>
          {tr("room_chat_is_off", undefined, "Chat is off")}
        </p>
      ) : mutedByHost ? (
        <p className={`text-xs ${muted}`} data-testid={`${testId}-muted`}>
          {tr("room_chat_host_muted", undefined, "The host muted chat for you")}
        </p>
      ) : (
        <>
          <div className="flex flex-wrap gap-1" role="group" aria-label={tr("room_quick", undefined, "Quick messages")} data-testid={`${testId}-quick`}>
            {QUICK_IDS.map((id) => (
              <button key={id} type="button" className={chip} onClick={() => void send({ quick: id })} data-testid={`${testId}-quick-${id}`}>
                {quickText(tr, id)}
              </button>
            ))}
          </div>
          {mode === "on" && (
            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                const t = cleanChat(text, { publicRoom });
                if (!t) return;
                void send({ text: t }).then(() => setText(""));
              }}
            >
              <input
                value={text}
                onChange={(e) => setText(e.target.value)}
                maxLength={MAX_CHAT}
                placeholder={tr("room_chat_type", undefined, "Type a message…")}
                aria-label={tr("room_chat_type", undefined, "Type a message…")}
                className={
                  dark
                    ? "min-w-0 flex-1 rounded-lg border border-white/15 bg-black px-3 py-1.5 text-sm outline-none focus:border-white/50"
                    : "min-w-0 flex-1 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-1.5 text-sm"
                }
                data-testid={`${testId}-input`}
              />
              <button type="submit" className={chip} aria-label={tr("room_chat_send", undefined, "Send")} data-testid={`${testId}-send`}>
                <Send size={14} />
              </button>
            </form>
          )}
          {slow && (
            <p className="text-xs text-amber-400" data-testid={`${testId}-slow`}>
              {tr("room_chat_slow", undefined, "Slow down")}
            </p>
          )}
        </>
      )}
    </section>
  );
}
