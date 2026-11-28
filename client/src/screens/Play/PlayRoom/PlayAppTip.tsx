import { useState } from "react";
import { Gamepad2, X } from "lucide-react";

import { safeGetItem, safeSetItem } from "../../../lib/safeStorage";
import { useTr } from "../../../state/lang";

const KEY = "ps5upload.tip.play_app_friend";

/** Shown until dismissed on this device (never a modal, never again once closed). */
export function tipShown(key: string): boolean {
  return safeGetItem(key) !== "1";
}

export function dismissTip(key: string): void {
  safeSetItem(key, "1");
}

/**
 * "Friend has a PS5? They can join with PS5Upload Play" (integration design, guidance rules): one
 * line on the Play Room screen with How; closing it is for good.
 */
export function PlayAppTip() {
  const tr = useTr();
  const [shown, setShown] = useState(() => tipShown(KEY));
  const [how, setHow] = useState(false);
  if (!shown) return null;
  return (
    <div className="flex items-start gap-2 rounded-lg bg-[var(--color-accent-soft)] px-3 py-2 text-xs" data-testid="room-play-app-tip">
      <Gamepad2 size={14} className="mt-0.5 shrink-0" aria-hidden />
      <div className="min-w-0 flex-1 space-y-1">
        <p>
          {tr("room_tip_play_app", undefined, "Friend has a PS5? They can join with PS5Upload Play.")}{" "}
          <button type="button" className="font-medium underline" onClick={() => setHow(!how)} aria-expanded={how} data-testid="room-play-app-tip-how">
            {tr("room_tip_how", undefined, "How")}
          </button>
        </p>
        {how && (
          <p className="text-[var(--color-muted)]">
            {tr(
              "room_tip_play_app_how",
              undefined,
              "They install PS5Upload Play on their PS5 (from ProsperoStore, or from PS5Upload on their own computer: Play › Play Room), open it and pick Join. Your room shows there when it's Friends or Public, or they scan your invite's QR code with their phone and tap Play on my PS5.",
            )}
          </p>
        )}
      </div>
      <button
        type="button"
        onClick={() => {
          dismissTip(KEY);
          setShown(false);
        }}
        aria-label={tr("rp_phone_close", undefined, "Close")}
        className="rounded p-0.5 hover:bg-[var(--color-surface-3)]"
        data-testid="room-play-app-tip-close"
      >
        <X size={14} />
      </button>
    </div>
  );
}
