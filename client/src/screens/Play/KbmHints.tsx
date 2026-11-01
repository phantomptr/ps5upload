// How to get the keyboard and mouse back from the PS5, shown where people look (the picture):
//   KbmCaptureCard  the first capture on this device, and again when someone seems to be trying
//                   to get out (useNativeKbm's escape signs): key caps of the release shortcut.
//   KbmReleaseChip  a faint "⌃ ⌥ to release" at the top while captured (Settings can hide it).
//   KbmToast        the mode switch's new mode, with the release key while captured.

import { useTr } from "../../state/lang";
import { useKbmStore } from "../../state/kbm";
import { shortcutLabel, shortcutParts, type Shortcut } from "../../lib/kbmShortcuts";
import type { KbmMode } from "../../lib/kbmInput";
import { kbmModeLabel } from "./useNativeKbm";

/** The shortcut as physical key caps. */
export function KeyCaps({ shortcut, size = "lg" }: { shortcut: Shortcut; size?: "lg" | "sm" }) {
  const parts = shortcutParts(shortcut);
  const cap =
    size === "lg"
      ? "min-w-[3.25rem] h-[3.25rem] px-3 text-2xl rounded-xl border-b-[5px]"
      : "min-w-[1.75rem] h-7 px-1.5 text-xs rounded-md border-b-[3px]";
  return (
    <span className="inline-flex items-center gap-1.5" data-testid="rp-keycaps">
      {parts.map((p, i) => (
        <kbd
          key={`${p}${i}`}
          className={`inline-flex items-center justify-center border border-white/25 border-b-white/40 bg-gradient-to-b from-white/20 to-white/5 font-sans font-semibold text-white shadow-[0_2px_0_rgba(0,0,0,0.35)] ${cap}`}
        >
          {p}
        </kbd>
      ))}
    </span>
  );
}

export function KbmCaptureCard({ reason, onDone }: { reason: "first" | "attempt"; onDone: () => void }) {
  const tr = useTr();
  const release = useKbmStore((s) => s.shortcuts.release);
  const key = shortcutLabel(release);
  return (
    <div className="pointer-events-none absolute inset-0 z-30 flex items-center justify-center p-4">
      <div
        role="status"
        aria-live="polite"
        className="rp-kbm-card pointer-events-auto flex w-[min(26rem,100%)] flex-col items-center gap-4 rounded-3xl border border-white/15 bg-neutral-950/85 px-6 py-5 text-center text-white shadow-2xl backdrop-blur-md"
        data-testid="rp-kbm-card"
        data-reason={reason}
        onClick={(e) => e.stopPropagation()}
        onDoubleClick={(e) => e.stopPropagation()}
      >
        <KeyCaps shortcut={release} />
        <p className="text-sm leading-6 text-white/90">
          {reason === "first"
            ? tr(
                "rp_kbm_card_first",
                { key },
                `Your keyboard & mouse now control the PS5. Press ${key} to get them back.`,
              )
            : tr("rp_kbm_card_again", { key }, `To get your keyboard & mouse back, press ${key}.`)}
        </p>
        <button
          type="button"
          onClick={onDone}
          className="rounded-full bg-white px-4 py-1.5 text-sm font-medium text-black hover:bg-white/90"
          data-testid="rp-kbm-card-ok"
        >
          {tr("rp_kbm_card_ok", undefined, "Got it")}
        </button>
      </div>
    </div>
  );
}

/** `low`: under the stream bar while it shows. */
export function KbmReleaseChip({ low = false }: { low?: boolean }) {
  const tr = useTr();
  const key = useKbmStore((s) => shortcutLabel(s.shortcuts.release));
  return (
    <div
      className={`pointer-events-none absolute left-1/2 z-10 -translate-x-1/2 rounded-full bg-black/45 px-3 py-1 text-xs text-white/70 ${low ? "top-16" : "top-3"}`}
      data-testid="rp-kbm-chip"
    >
      {tr("rp_kbm_chip", { key }, `${key} to release`)}
    </div>
  );
}

export function KbmToast({ mode, captured }: { mode: KbmMode; captured: boolean }) {
  const tr = useTr();
  const key = useKbmStore((s) => shortcutLabel(s.shortcuts.release));
  return (
    <div
      role="status"
      className="pointer-events-none absolute left-1/2 top-24 z-30 -translate-x-1/2 rounded-full bg-black/80 px-4 py-2 text-sm text-white shadow-lg"
      data-testid="rp-kbm-toast"
    >
      <span className="font-medium">{kbmModeLabel(tr, mode)}</span>
      {captured && (
        <span className="text-white/70">
          {" · "}
          {tr("rp_kbm_chip", { key }, `${key} to release`)}
        </span>
      )}
    </div>
  );
}
