import { Button } from "../../components";
import { effectiveMode, kbmBlockReason, KBM_MODES, modeUnavailable, type KbmMode } from "../../lib/kbmInput";
import { useTr } from "../../state/lang";
import { useGameControlsStore } from "../../state/gameControls";
import { rememberedMode, useKbmStore } from "../../state/kbm";
import { chooseKbmMode, kbmBlockText, kbmModeLabel } from "./useNativeKbm";

/** Settings › Controls: how this computer's keyboard and mouse play the running game (or the
 *  home screen), and what the PS5 sees (lib/kbmInput.ts). */
export function KbmModeSection({ running }: { running: boolean }) {
  const tr = useTr();
  const title = useGameControlsStore((s) => s.title);
  const modes = useKbmStore((s) => s.modes);
  const caps = useKbmStore((s) => s.caps);
  const status = useKbmStore((s) => s.status);
  const hintNow = useKbmStore((s) => s.hintNow);
  const remembered = rememberedMode(modes, title);
  const mode = effectiveMode(remembered, caps);
  const label: Record<KbmMode, string> = {
    controller: kbmModeLabel(tr, "controller"),
    native: kbmModeLabel(tr, "native"),
    mixed: kbmModeLabel(tr, "mixed"),
  };
  const what: Record<KbmMode, string> = {
    controller: tr("rp_kbm_controller_hint", undefined, "Keys and mouse act as a controller (the key list below)."),
    native: tr("rp_kbm_native_hint", undefined, "The PS5 gets a real keyboard and mouse, for games that support them."),
    mixed: tr("rp_kbm_mixed_hint", undefined, "A real mouse; keys act as a controller (WASD moves)."),
  };
  const unsupported = !caps || (!caps.mouse && !caps.keyboard);
  const why = kbmBlockText(tr, kbmBlockReason(caps));
  const on = running && mode !== "controller";
  const mark = (v: boolean | undefined) => (v ? "✓" : "–");
  return (
    <section className="space-y-2" data-testid="rp-kbm">
      <h3 className="text-sm font-semibold">{tr("rp_kbm_title", undefined, "Keyboard & mouse")}</h3>
      <div className="flex flex-wrap gap-2" role="radiogroup" aria-label={tr("rp_kbm_title", undefined, "Keyboard & mouse")}>
        {KBM_MODES.map((m) => {
          const off = modeUnavailable(m, caps);
          return (
            <Button
              key={m}
              role="radio"
              aria-checked={mode === m}
              variant={mode === m ? "primary" : "secondary"}
              disabled={!!off}
              title={off ? why : undefined}
              onClick={() => chooseKbmMode(title, m)}
              data-testid={`rp-kbm-mode-${m}`}
            >
              {label[m]}
            </Button>
          );
        })}
      </div>
      <p className="text-xs text-[var(--color-muted)]">
        {what[mode]}{" "}
        {tr("rp_kbm_hotkey", undefined, "Ctrl+Alt+M switches.")}
      </p>
      {hintNow && mode !== "controller" && (
        <p className="text-xs" data-testid="rp-kbm-hint">
          {tr("rp_kbm_hint", undefined, "If the game ignores the mouse, switch back to Controller.")}
        </p>
      )}
      {unsupported && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="rp-kbm-unavailable">
          {why}
        </p>
      )}
      {remembered !== mode && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="rp-kbm-fallback">
          {tr("rp_kbm_fallback", { mode: label[remembered] }, `Playing with Controller: ${label[remembered]} comes back when this console supports it.`)}
        </p>
      )}
      <p className="text-xs" data-testid="rp-kbm-status">
        {tr(
          "rp_kbm_sees",
          { mouse: mark(on && status?.mouse), keyboard: mark(on && status?.keyboard) },
          `PS5 sees: mouse ${mark(on && status?.mouse)} keyboard ${mark(on && status?.keyboard)}`,
        )}
      </p>
      <p className="text-xs text-[var(--color-muted)]">
        {title
          ? tr("rp_kbm_per_game", { title }, `Kept for ${title}.`)
          : tr("rp_kbm_home", undefined, "Kept for the home screen; each game keeps its own.")}
      </p>
    </section>
  );
}
