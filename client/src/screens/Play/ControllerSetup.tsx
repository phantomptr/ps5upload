import { useEffect, useRef, useState } from "react";
import { Gamepad2 } from "lucide-react";

import { Button, Select } from "../../components";
import {
  addExtra,
  applyStep,
  blankProfile,
  detectAxis,
  detectPress,
  loadProfiles,
  MAP_STEPS,
  profileFor,
  saveProfile,
  snapshot,
  withStandardGaps,
  type MapStep,
  type PadSnapshot,
} from "../../lib/padProfiles";
import { gamepadsOf } from "../../lib/nativePad";
import { setPadForwardReason } from "../../lib/androidPadBridge";
import { PadButton, type PadProfile } from "../../lib/remotePlayStream";
import { useTr } from "../../state/lang";
import { rpInput } from "../../state/remotePlay";

type Tr = ReturnType<typeof useTr>;

/** The controller at `index` (the browser's, or the one the Android app forwards). */
const padAt = (index: number) => gamepadsOf(navigator).find((g) => g?.index === index) ?? null;

interface PadInfo {
  index: number;
  id: string;
  standard: boolean;
}

/** The controllers the browser sees. It shows one only after a button press on it (privacy). */
function useConnectedPads(): PadInfo[] {
  const read = (): PadInfo[] =>
    typeof navigator === "undefined"
      ? []
      : gamepadsOf(navigator)
          .filter((g): g is Gamepad => !!g?.connected)
          .map((g) => ({ index: g.index, id: g.id, standard: g.mapping === "standard" }));
  const [pads, setPads] = useState(read);
  // Android: the app forwards controllers only while something on the page reads them.
  useEffect(() => {
    setPadForwardReason("controller-setup", true);
    return () => setPadForwardReason("controller-setup", false);
  }, []);
  useEffect(() => {
    const update = () => setPads(read());
    window.addEventListener("gamepadconnected", update);
    window.addEventListener("gamepaddisconnected", update);
    const t = window.setInterval(update, 1000);
    return () => {
      window.removeEventListener("gamepadconnected", update);
      window.removeEventListener("gamepaddisconnected", update);
      window.clearInterval(t);
    };
  }, []);
  return pads;
}

/** A controller's name without the browser's vendor/product numbers. */
export function padName(id: string): string {
  const n = id.replace(/\s*\((?:STANDARD GAMEPAD\s*)?Vendor:.*\)\s*$/i, "").replace(/^[0-9a-f]{4}-[0-9a-f]{4}-/i, "");
  return n.trim() || id;
}

function stepText(tr: Tr, s: MapStep): string {
  if (s.kind === "axis") {
    if (s.which === "lx") return tr("rp_pad_step_lx", undefined, "Push the left stick right");
    if (s.which === "ly") return tr("rp_pad_step_ly", undefined, "Push the left stick down");
    if (s.which === "rx") return tr("rp_pad_step_rx", undefined, "Push the right stick right");
    return tr("rp_pad_step_ry", undefined, "Push the right stick down");
  }
  return tr("rp_pad_press", { name: s.name }, `Press ${s.name}`);
}

/** The PS5 buttons an extra button can be set to. */
const EXTRA_TARGETS: Array<[number, string]> = [
  [PadButton.CROSS, "✕"],
  [PadButton.CIRCLE, "○"],
  [PadButton.SQUARE, "□"],
  [PadButton.TRIANGLE, "△"],
  [PadButton.L1, "L1"],
  [PadButton.R1, "R1"],
  [PadButton.L3, "L3"],
  [PadButton.R3, "R3"],
  [PadButton.UP, "↑"],
  [PadButton.DOWN, "↓"],
  [PadButton.LEFT, "←"],
  [PadButton.RIGHT, "→"],
  [PadButton.OPTIONS, "Options"],
  [PadButton.SHARE, "Create"],
  [PadButton.TOUCHPAD, "Touchpad"],
  [PadButton.PS, "PS"],
];

const atRest = (rest: PadSnapshot, now: PadSnapshot) =>
  now.buttons.every((v, i) => Math.abs(v - (rest.buttons[i] ?? 0)) < 0.3) &&
  now.axes.every((v, i) => Math.abs(v - (rest.axes[i] ?? 0)) < 0.3);

/** Every frame while `on`: the controller at `index`, as a snapshot (null when it is gone). */
function usePadFrames(on: boolean, index: number, cb: (s: PadSnapshot | null) => void) {
  const ref = useRef(cb);
  useEffect(() => {
    ref.current = cb;
  });
  useEffect(() => {
    if (!on) return;
    let raf = 0;
    const tick = () => {
      const g = padAt(index);
      ref.current(g?.connected ? snapshot(g) : null);
      raf = window.requestAnimationFrame(tick);
    };
    raf = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(raf);
  }, [on, index]);
}

type Mode = { kind: "idle" } | { kind: "map"; pad: PadInfo } | { kind: "extra"; pad: PadInfo };

/** Controllers: which are connected, whether each is recognized, and mapping any of them by
 *  pressing its buttons in turn. Phone controllers (Backbone, Kishi, GameSir, Nomad...) are
 *  controllers like any other. */
export function ControllerSetup() {
  const tr = useTr();
  const pads = useConnectedPads();
  const [saved, setSaved] = useState(loadProfiles);
  const [mode, setMode] = useState<Mode>({ kind: "idle" });
  const [notice, setNotice] = useState<string | null>(null);

  // A controller being mapped plays nothing on the console.
  useEffect(() => {
    rpInput.mapping = mode.kind !== "idle";
    return () => {
      rpInput.mapping = false;
    };
  }, [mode.kind]);

  const done = (id: string, prof: PadProfile | null, text: string) => {
    setSaved(saveProfile(id, prof));
    setMode({ kind: "idle" });
    setNotice(text);
  };

  return (
    <div className="space-y-2" data-testid="rp-pads">
      {pads.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]">
          {tr("rp_pads_none", undefined, "No controller seen yet. Connect one and press any button on it.")}
        </p>
      ) : (
        <ul className="space-y-2">
          {pads.map((p) => {
            const mapped = !!saved[p.id];
            return (
              <li
                key={`${p.index}-${p.id}`}
                className="flex flex-wrap items-center gap-2 rounded-[var(--radius-field)] border border-[var(--color-border)] px-3 py-2"
                data-testid="rp-pad"
              >
                <Gamepad2 size={16} className="shrink-0 text-[var(--color-muted)]" />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-sm font-medium">{padName(p.id)}</div>
                  <div className="text-xs text-[var(--color-muted)]">
                    {mapped
                      ? tr("rp_pad_mapped", undefined, "Plays with your mapping")
                      : p.standard
                        ? tr("rp_pad_standard", undefined, "Recognized")
                        : tr(
                            "rp_pad_guess",
                            undefined,
                            "Not recognized: it plays in the usual layout. If a button is wrong, map them.",
                          )}
                  </div>
                </div>
                {mode.kind === "idle" && (
                  <div className="flex flex-wrap gap-2">
                    <Button size="sm" variant={p.standard || mapped ? "secondary" : "primary"} onClick={() => setMode({ kind: "map", pad: p })}>
                      {tr("rp_pad_map", undefined, "Map buttons")}
                    </Button>
                    <Button size="sm" variant="secondary" onClick={() => setMode({ kind: "extra", pad: p })}>
                      {tr("rp_pad_extra", undefined, "Set an extra button")}
                    </Button>
                    {mapped && (
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => done(p.id, null, tr("rp_pad_reset_done", undefined, "Back to the usual layout."))}
                      >
                        {tr("rp_pad_reset", undefined, "Reset")}
                      </Button>
                    )}
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {mode.kind === "map" && (
        <MapWizard
          pad={mode.pad}
          onCancel={() => setMode({ kind: "idle" })}
          onDone={(prof) => done(mode.pad.id, prof, tr("rp_pad_saved", undefined, "Saved: this controller plays with your mapping."))}
        />
      )}
      {mode.kind === "extra" && (
        <ExtraButton
          pad={mode.pad}
          onCancel={() => setMode({ kind: "idle" })}
          onDone={(prof) => done(mode.pad.id, prof, tr("rp_pad_extra_done", undefined, "Saved: the extra button is set."))}
        />
      )}
      {notice && mode.kind === "idle" && (
        <p className="text-xs text-[var(--color-good)]" role="status">
          {notice}
        </p>
      )}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_pads_hint",
          undefined,
          "Phone controllers such as Backbone, Razer Kishi, GameSir or Scuf Nomad work like any controller: connect it, press a button, and it plays.",
        )}
      </p>
    </div>
  );
}

/** Press each button in turn; a step can be skipped (it keeps the usual layout). */
function MapWizard({ pad, onCancel, onDone }: { pad: PadInfo; onCancel: () => void; onDone: (p: PadProfile) => void }) {
  const tr = useTr();
  const [step, setStep] = useState(0);
  const prof = useRef<PadProfile>(blankProfile());
  const rest = useRef<PadSnapshot | null>(null);
  /** After an answer: wait until everything is let go before the next step listens. */
  const waiting = useRef(true);
  const [gone, setGone] = useState(false);

  const advance = (to: number) => {
    waiting.current = true;
    if (to >= MAP_STEPS.length) onDone(withStandardGaps(prof.current));
    else setStep(to);
  };

  usePadFrames(true, pad.index, (now) => {
    setGone(!now);
    if (!now) return;
    if (!rest.current) {
      // Where everything rests, taken as the wizard opens (triggers on axes rest at -1).
      rest.current = now;
      waiting.current = false;
      return;
    }
    if (waiting.current) {
      if (atRest(rest.current, now)) waiting.current = false;
      return;
    }
    const s = MAP_STEPS[step];
    const got = s.kind === "axis" ? detectAxis(rest.current!, now) : detectPress(rest.current!, now);
    if (got) {
      prof.current = applyStep(prof.current, s, got);
      advance(step + 1);
    }
  });

  return (
    <div className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-accent)] p-3" data-testid="rp-pad-wizard">
      <div className="text-xs text-[var(--color-muted)]">
        {padName(pad.id)} · {tr("rp_pad_progress", { n: step + 1, total: MAP_STEPS.length }, `${step + 1} of ${MAP_STEPS.length}`)}
      </div>
      <div className="text-lg font-semibold" data-testid="rp-pad-step">
        {stepText(tr, MAP_STEPS[step])}
      </div>
      {gone && (
        <p className="text-xs text-[var(--color-warn)]">
          {tr("rp_pad_gone", undefined, "The controller went away. Press a button on it.")}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="secondary" onClick={() => advance(step + 1)} data-testid="rp-pad-skip">
          {tr("rp_pad_skip", undefined, "Skip")}
        </Button>
        {step > 0 && (
          <Button size="sm" variant="ghost" onClick={() => advance(step - 1)}>
            {tr("rp_pad_back", undefined, "Back")}
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {tr("rp_pad_cancel", undefined, "Cancel")}
        </Button>
      </div>
    </div>
  );
}

/** Press the extra button, then pick what it presses on the PS5. */
function ExtraButton({ pad, onCancel, onDone }: { pad: PadInfo; onCancel: () => void; onDone: (p: PadProfile) => void }) {
  const tr = useTr();
  const rest = useRef<PadSnapshot | null>(null);
  const [got, setGot] = useState<ReturnType<typeof detectPress>>(null);
  const [target, setTarget] = useState<number>(PadButton.TOUCHPAD);

  usePadFrames(!got, pad.index, (now) => {
    if (!now) return;
    if (!rest.current) {
      rest.current = now;
      return;
    }
    const src = detectPress(rest.current, now);
    if (src) setGot(src);
  });

  return (
    <div className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-accent)] p-3" data-testid="rp-pad-extra">
      <div className="text-sm font-semibold">
        {got
          ? tr("rp_pad_extra_got", undefined, "Got it. What should it press?")
          : tr("rp_pad_extra_press", undefined, "Press the extra button (a back button or paddle)")}
      </div>
      {got && (
        <Select
          label={tr("rp_pad_extra_as", undefined, "It presses")}
          block={false}
          className="w-40"
          value={String(target)}
          onChange={(e) => setTarget(Number(e.target.value))}
          options={EXTRA_TARGETS.map(([bit, name]) => ({ value: String(bit), label: name }))}
        />
      )}
      <div className="flex flex-wrap gap-2">
        {got && (
          <Button
            size="sm"
            variant="primary"
            onClick={() => {
              const g = padAt(pad.index);
              const base = g ? profileFor(g) : withStandardGaps(blankProfile());
              onDone(addExtra(base, target, got));
            }}
          >
            {tr("rp_pad_extra_save", undefined, "Save")}
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {tr("rp_pad_cancel", undefined, "Cancel")}
        </Button>
      </div>
    </div>
  );
}
