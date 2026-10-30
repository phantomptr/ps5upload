import { useState } from "react";
import { Plus, Trash2 } from "lucide-react";

import { Toggle } from "../../../components";
import {
  DEFAULT_CONTROLS,
  L2_BIT,
  R2_BIT,
  TURBO_BUTTONS,
  type GameControls,
  type Macro,
} from "../../../lib/gameControls";
import { useGameControlsStore } from "../../../state/gameControls";
import { useTr } from "../../../state/lang";

const BUTTONS = [...TURBO_BUTTONS, { bit: L2_BIT, name: "L2" }, { bit: R2_BIT, name: "R2" }];

/** A key as people say it: "Q", "Space", "F1". */
const keyName = (code: string) => code.replace(/^Key/, "").replace(/^Digit/, "");

/** Game panel › Controls: this game's turbo buttons, mouse aim and macros (kept per game on
 *  this device; they switch by themselves with the running game). */
export function ControlsTab({ name }: { name: (id: string) => string }) {
  const tr = useTr();
  const title = useGameControlsStore((s) => s.title);
  const c = useGameControlsStore((s) => (s.title ? (s.byTitle[s.title] ?? DEFAULT_CONTROLS) : DEFAULT_CONTROLS));
  const update = useGameControlsStore((s) => s.update);
  const reset = useGameControlsStore((s) => s.reset);
  const [listening, setListening] = useState<number | null>(null);

  if (!title) {
    return (
      <p className="py-4 text-white/70" data-testid="rp-controls-none">
        {tr("rp_controls_no_game", undefined, "Start a game to set its controls. Each game keeps its own.")}
      </p>
    );
  }
  const set = (p: Partial<GameControls>) => update(title, p);
  const setMacro = (i: number, m: Macro | null) =>
    set({ macros: m ? c.macros.map((x, j) => (j === i ? m : x)) : c.macros.filter((_, j) => j !== i) });

  return (
    <div
      className="space-y-5 py-1"
      data-testid="rp-controls"
      onKeyDown={(e) => {
        if (listening === null) return;
        e.preventDefault();
        if (e.code !== "Escape") setMacro(listening, { ...c.macros[listening], key: e.code });
        setListening(null);
      }}
    >
      <p className="text-xs text-white/60">
        {tr("rp_controls_for", { name: name(title) }, `For ${name(title)}; each game keeps its own.`)}
      </p>

      <section className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-white/60">{tr("rp_controls_turbo", undefined, "Turbo")}</h3>
        <p className="text-xs text-white/60">
          {tr("rp_controls_turbo_hint", undefined, "Holding a turbo button presses it again and again.")}
        </p>
        <div className="flex flex-wrap gap-1.5" data-testid="rp-controls-turbo">
          {BUTTONS.map((b) => {
            const on = (c.turbo & b.bit) !== 0;
            return (
              <button
                key={b.bit}
                type="button"
                aria-pressed={on}
                onClick={() => set({ turbo: on ? c.turbo & ~b.bit : c.turbo | b.bit })}
                className={`h-8 min-w-10 rounded-full px-2.5 text-xs font-medium ${on ? "bg-[var(--color-accent)] text-white" : "bg-white/10 hover:bg-white/20"}`}
              >
                {b.name}
              </button>
            );
          })}
        </div>
        <label className="flex items-center gap-3 text-xs">
          <span className="w-28 text-white/70">{tr("rp_controls_rate", { n: c.turboHz }, `${c.turboHz} presses a second`)}</span>
          <input
            type="range"
            min={2}
            max={30}
            value={c.turboHz}
            onChange={(e) => set({ turboHz: Number(e.target.value) })}
            className="flex-1 accent-white"
            aria-label={tr("rp_controls_rate_label", undefined, "Turbo speed")}
          />
        </label>
      </section>

      <section className="space-y-2">
        <Toggle
          checked={c.mouse}
          onChange={(v) => set({ mouse: v })}
          label={tr("rp_controls_mouse", undefined, "Mouse aim")}
          hint={tr(
            "rp_controls_mouse_hint",
            undefined,
            "Click the picture to capture the pointer: moving the mouse moves the right stick, left click is R2, right click is L2. Esc lets the pointer go.",
          )}
        />
        {c.mouse && (
          <>
            <label className="flex items-center gap-3 text-xs">
              <span className="w-28 text-white/70">{tr("rp_controls_sens", undefined, "Sensitivity")}</span>
              <input
                type="range"
                min={0.2}
                max={3}
                step={0.1}
                value={c.mouseSens}
                onChange={(e) => set({ mouseSens: Number(e.target.value) })}
                className="flex-1 accent-white"
                aria-label={tr("rp_controls_sens", undefined, "Sensitivity")}
              />
            </label>
            <Toggle
              checked={c.mouseInvertY}
              onChange={(v) => set({ mouseInvertY: v })}
              label={tr("rp_controls_invert", undefined, "Invert up and down")}
            />
          </>
        )}
      </section>

      <section className="space-y-2">
        <h3 className="text-xs font-semibold uppercase tracking-wide text-white/60">{tr("rp_controls_macros", undefined, "Macros")}</h3>
        <p className="text-xs text-white/60">
          {tr(
            "rp_controls_macros_hint",
            undefined,
            "A key plays a few presses in a row. Pressing one of its buttons yourself stops it.",
          )}
        </p>
        {c.macros.map((m, i) => (
          <div key={i} className="space-y-1.5 rounded-lg bg-white/5 p-2" data-testid="rp-controls-macro">
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={() => setListening(i)}
                className="h-7 rounded-full bg-white/10 px-3 text-xs hover:bg-white/20"
                data-testid="rp-controls-macro-key"
              >
                {listening === i
                  ? tr("rp_controls_press_key", undefined, "Press a key…")
                  : m.key
                    ? tr("rp_controls_key", { key: keyName(m.key) }, `Key: ${keyName(m.key)}`)
                    : tr("rp_controls_pick_key", undefined, "Choose its key")}
              </button>
              <span className="flex-1" />
              <button
                type="button"
                onClick={() => setMacro(i, null)}
                aria-label={tr("rp_controls_macro_remove", undefined, "Remove this macro")}
                className="flex h-7 w-7 items-center justify-center rounded-full hover:bg-white/15"
              >
                <Trash2 size={13} />
              </button>
            </div>
            {m.steps.map((st, k) => (
              <div key={k} className="flex items-center gap-1.5 text-xs">
                <select
                  value={st.buttons}
                  onChange={(e) =>
                    setMacro(i, { ...m, steps: m.steps.map((x, j) => (j === k ? { ...x, buttons: Number(e.target.value) } : x)) })
                  }
                  className="h-7 rounded-md bg-white/10 px-1.5 text-white"
                  aria-label={tr("rp_controls_step_button", undefined, "Button")}
                >
                  <option value={0}>{tr("rp_controls_wait", undefined, "Wait")}</option>
                  {BUTTONS.map((b) => (
                    <option key={b.bit} value={b.bit}>
                      {b.name}
                    </option>
                  ))}
                </select>
                <input
                  type="number"
                  min={16}
                  max={10000}
                  step={10}
                  value={st.ms}
                  onChange={(e) =>
                    setMacro(i, {
                      ...m,
                      steps: m.steps.map((x, j) => (j === k ? { ...x, ms: Math.max(16, Number(e.target.value) || 16) } : x)),
                    })
                  }
                  className="h-7 w-20 rounded-md bg-white/10 px-1.5 text-white"
                  aria-label={tr("rp_controls_step_ms", undefined, "Milliseconds")}
                />
                <span className="text-white/60">ms</span>
                <button
                  type="button"
                  onClick={() => setMacro(i, { ...m, steps: m.steps.filter((_, j) => j !== k) })}
                  aria-label={tr("rp_controls_step_remove", undefined, "Remove this step")}
                  className="flex h-7 w-7 items-center justify-center rounded-full hover:bg-white/15"
                >
                  <Trash2 size={12} />
                </button>
              </div>
            ))}
            {m.steps.length < 32 && (
              <button
                type="button"
                onClick={() => setMacro(i, { ...m, steps: [...m.steps, { buttons: TURBO_BUTTONS[0].bit, ms: 100 }] })}
                className="flex h-7 items-center gap-1 rounded-full px-2 text-xs hover:bg-white/10"
              >
                <Plus size={12} />
                {tr("rp_controls_step_add", undefined, "Add a step")}
              </button>
            )}
          </div>
        ))}
        {c.macros.length < 12 && (
          <button
            type="button"
            onClick={() => {
              set({ macros: [...c.macros, { key: "", steps: [{ buttons: TURBO_BUTTONS[0].bit, ms: 100 }] }] });
              setListening(c.macros.length);
            }}
            className="flex h-8 items-center gap-1.5 rounded-full bg-white/10 px-3 text-xs hover:bg-white/20"
            data-testid="rp-controls-macro-add"
          >
            <Plus size={13} />
            {tr("rp_controls_macro_add", undefined, "Add a macro")}
          </button>
        )}
      </section>

      <button
        type="button"
        onClick={() => reset(title)}
        className="text-xs text-white/60 underline-offset-2 hover:underline"
      >
        {tr("rp_controls_reset", undefined, "Clear this game's controls")}
      </button>
    </div>
  );
}
