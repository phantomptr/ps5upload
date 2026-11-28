import type { ReactNode } from "react";
import { Lock } from "lucide-react";

import { regionLabel } from "../lib/room/regions";
import { versionForProto, type CardAction, type Expected, type RoomCard } from "../lib/room/rooms";
import { languageName } from "../lib/languageNames";
import { useTr } from "../state/lang";
import { PingBars } from "./PingBars";

type Tr = ReturnType<typeof useTr>;

/** A card's button in words (design 7.0). */
export function cardActionText(tr: Tr, a: CardAction, proto = 2): string {
  switch (a) {
    case "join":
      return tr("rooms_join", undefined, "Join");
    case "ask":
      return tr("rooms_ask", undefined, "Ask to join");
    case "full":
      return tr("rooms_full", undefined, "Full");
    case "update": {
      const v = versionForProto(Math.max(2, proto));
      return tr("rooms_update_to_join", { v }, `Update to join (needs PS5Upload ${v} or newer)`);
    }
    case "host_old":
      return tr("rooms_host_old", undefined, "The host needs to update PS5Upload");
  }
}

/**
 * One room card (design 7.0), never more than three lines of facts:
 * `[cover] Room name · Host · tag` / `Game · 2/4 · 🔒 · English · Europe West` / ping + buttons.
 * Used by the app's Rooms browser and the website's Rooms page (`dark`).
 */
export function RoomCardView({
  card,
  expected,
  action,
  onAction,
  onDetails,
  cover,
  dark = false,
  extra,
  focused = false,
}: {
  card: RoomCard;
  expected: Expected | null;
  action: CardAction;
  onAction: () => void;
  onDetails?: () => void;
  /** The cover, drawn by this device from its own artwork (never from the hub). */
  cover?: ReactNode;
  dark?: boolean;
  /** A line under the facts (friends: "Locked"). */
  extra?: ReactNode;
  focused?: boolean;
}) {
  const tr = useTr();
  const muted = dark ? "text-white/60" : "text-[var(--color-muted)]";
  const facts = [
    card.game?.title ?? null,
    `${card.inRoom}/${card.size}`,
    languageName(card.lang),
    card.region ? regionLabel(tr, card.region) : null,
  ].filter(Boolean);
  const disabled = action === "full" || action === "update" || action === "host_old";
  const btn = dark
    ? "rounded-full bg-white px-3 py-1.5 text-xs font-semibold text-black disabled:opacity-40"
    : "rounded-full bg-[var(--color-accent)] px-3 py-1.5 text-xs font-semibold text-[var(--color-on-accent,#fff)] disabled:opacity-40";
  const ghost = dark ? "rounded-full bg-white/15 px-3 py-1.5 text-xs" : "rounded-full border border-[var(--color-border)] px-3 py-1.5 text-xs hover:bg-[var(--color-surface-3)]";
  return (
    <li
      className={`flex gap-3 rounded-lg p-2.5 ${dark ? "bg-white/5" : "border border-[var(--color-border)] bg-[var(--color-surface)]"} ${
        focused ? "ring-2 ring-[var(--color-accent)]" : ""
      } ${action === "full" ? "opacity-70" : ""}`}
      data-testid="room-card"
      data-room={card.room}
      data-action={action}
    >
      <div className={`flex h-14 w-14 shrink-0 items-center justify-center overflow-hidden rounded-md ${dark ? "bg-white/10" : "bg-[var(--color-surface-3)]"}`}>
        {cover ?? <span className="text-lg font-bold opacity-60">{(card.game?.title ?? card.name).slice(0, 1).toUpperCase()}</span>}
      </div>
      <div className="min-w-0 flex-1 space-y-0.5">
        <p className="truncate text-sm font-semibold" data-testid="room-card-title">
          {card.name}
          <span className={`font-normal ${muted}`}>
            {" · "}
            {card.host}
            {card.tag ? ` ${card.tag.toUpperCase()}` : ""}
          </span>
        </p>
        <p className={`flex flex-wrap items-center gap-x-1.5 text-xs ${muted}`} data-testid="room-card-facts">
          {facts.join(" · ")}
          {card.pw && (
            <span title={tr("rooms_needs_password", undefined, "Needs a password")} data-testid="room-card-lock">
              <Lock size={11} aria-label={tr("rooms_needs_password", undefined, "Needs a password")} />
            </span>
          )}
          {action === "full" && <span className="font-medium">{tr("rooms_full", undefined, "Full")}</span>}
        </p>
        {extra}
        <div className="flex flex-wrap items-center gap-2 pt-0.5">
          {expected ? (
            <PingBars
              tier={expected.tier}
              ms={expected.ms}
              estimate={expected.kind === "about"}
              measured={expected.kind === "measured"}
              lastTime={expected.kind === "last_time"}
              compact
              testId="room-card-ping"
            />
          ) : (
            <span className={`text-xs ${muted}`}>—</span>
          )}
          <span className="ms-auto flex gap-1.5">
            {onDetails && (
              <button type="button" className={ghost} onClick={onDetails} data-testid="room-card-details">
                {tr("rooms_details", undefined, "Details")}
              </button>
            )}
            <button type="button" className={btn} disabled={disabled} onClick={onAction} data-testid="room-card-action">
              {cardActionText(tr, action, card.proto)}
            </button>
          </span>
        </div>
      </div>
    </li>
  );
}
