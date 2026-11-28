import { useEffect, useRef, useState } from "react";
import { Gamepad2 } from "lucide-react";

import { Button, Select } from "../../components";
import {
  addExtra,
  blankProfile,
  deadzonesById,
  defaultProfileFor,
  detectPress,
  loadDeadzones,
  loadProfiles,
  MAP_STEPS,
  PadLearner,
  padKind,
  profileFor,
  saveDeadzones,
  saveProfile,
  snapshot,
  withStandardGaps,
  type MapStep,
  type PadSnapshot,
} from "../../lib/padProfiles";
import { isWeak8BitDo, padName } from "../../lib/padMappings";
import { EightBitDoHint } from "./PadHint";
import { gamepadsOf } from "../../lib/nativePad";
import { setPadForwardReason } from "../../lib/androidPadBridge";
import {
  axisValue,
  MAX_STICK_DEADZONE,
  PadButton,
  USUAL_STICK_DEADZONE,
  type PadProfile,
  type StickDeadzones,
} from "../../lib/remotePlayStream";
import { useTr } from "../../state/lang";
import { rpInput } from "../../state/remotePlay";

type Tr = ReturnType<typeof useTr>;

/** The controller at `index` (the browser's, or the one the Android app forwards). */
const padAt = (index: number) => gamepadsOf(navigator).find((g) => g?.index === index) ?? null;

interface PadInfo {
  index: number;
  id: string;
  /** Laid out the standard way by the browser, or from the known controllers' table. */
  standard: boolean;
  /** An 8BitDo in a mode the browser does not lay out the standard way. */
  weak8BitDo: boolean;
}

/** The controllers the browser sees. It shows one only after a button press on it (privacy). */
function useConnectedPads(): PadInfo[] {
  const read = (): PadInfo[] =>
    typeof navigator === "undefined"
      ? []
      : gamepadsOf(navigator)
          .filter((g): g is Gamepad => !!g?.connected)
          .map((g) => {
            const kind = padKind(g);
            return { index: g.index, id: g.id, standard: kind === "standard" || kind === "known", weak8BitDo: isWeak8BitDo(g) };
          });
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

type Mode =
  | { kind: "idle" }
  | { kind: "map"; pad: PadInfo }
  | { kind: "extra"; pad: PadInfo }
  | { kind: "deadzone"; pad: PadInfo };

/** A dead zone as a whole percent. */
const pct = (v: number) => Math.round(v * 100);

/** Controllers: which are connected, whether each is recognized, and mapping any of them by
 *  pressing its buttons in turn. Phone controllers (Backbone, Kishi, GameSir, Nomad...) are
 *  controllers like any other. */
export function ControllerSetup() {
  const tr = useTr();
  const pads = useConnectedPads();
  const [saved, setSaved] = useState(loadProfiles);
  const [savedDz, setSavedDz] = useState(loadDeadzones);
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
            const dz = deadzonesById(savedDz, p.id);
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
                    {p.weak8BitDo && !mapped && (
                      <EightBitDoHint id={p.id} className="block" />
                    )}
                    {mapped
                      ? tr("rp_pad_mapped", undefined, "Plays with your mapping")
                      : p.standard
                        ? tr("rp_pad_standard", undefined, "Recognized")
                        : tr(
                            "rp_pad_guess",
                            undefined,
                            "Not recognized: it plays in the usual layout. If a button is wrong, map them.",
                          )}
                    {dz && (
                      <span className="block" data-testid="rp-pad-deadzone-status">
                        {tr(
                          "rp_pad_deadzone_status",
                          { left: pct(dz.left), right: pct(dz.right) },
                          `Stick dead zones: left ${pct(dz.left)}%, right ${pct(dz.right)}%`,
                        )}
                      </span>
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
                    <Button size="sm" variant="secondary" onClick={() => setMode({ kind: "deadzone", pad: p })} data-testid="rp-pad-deadzone">
                      {tr("rp_pad_deadzone", undefined, "Stick dead zones")}
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
      {mode.kind === "deadzone" && (
        <DeadzoneEditor
          pad={mode.pad}
          initial={deadzonesById(savedDz, mode.pad.id) ?? { left: 0, right: 0 }}
          onCancel={() => setMode({ kind: "idle" })}
          onDone={(dz) => {
            setSavedDz(saveDeadzones(mode.pad.id, dz));
            setMode({ kind: "idle" });
            setNotice(
              dz.left > 0 || dz.right > 0
                ? tr("rp_pad_deadzone_saved", undefined, "Saved: this controller plays with its own stick dead zones.")
                : tr("rp_pad_deadzone_off", undefined, "Stick dead zones are back to the usual."),
            );
          }}
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

/** Press each button in turn; a step can be skipped (it keeps what the controller had). */
function MapWizard({ pad, onCancel, onDone }: { pad: PadInfo; onCancel: () => void; onDone: (p: PadProfile) => void }) {
  const tr = useTr();
  const learner = useRef(new PadLearner(MAP_STEPS));
  const [step, setStep] = useState(0);
  const [gone, setGone] = useState(false);

  const sync = () => {
    const l = learner.current;
    if (!l.done) return setStep(l.step);
    const g = padAt(pad.index);
    onDone(l.profile(g ? defaultProfileFor(g) : undefined));
  };
  const advance = (to: number) => {
    if (to < step) learner.current.back();
    else learner.current.skip();
    sync();
  };

  usePadFrames(true, pad.index, (now) => {
    setGone(!now);
    if (now && learner.current.feed(now)) sync();
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

/** How far out (0-1) each stick of the controller at `index` is now, read through its profile;
 *  null when it is gone. Updated every frame while mounted. */
function useStickReach(index: number): StickDeadzones | null {
  const [reach, setReach] = useState<StickDeadzones | null>(null);
  useEffect(() => {
    let raf = 0;
    const tick = () => {
      const g = padAt(index);
      if (!g?.connected) setReach(null);
      else {
        const prof = profileFor(g);
        const mag = (ax: PadProfile["lx"], ay: PadProfile["ly"]) =>
          Math.min(1, Math.hypot(ax ? axisValue(g, ax) : 0, ay ? axisValue(g, ay) : 0));
        const left = Math.round(mag(prof.lx, prof.ly) * 100) / 100;
        const right = Math.round(mag(prof.rx, prof.ry) * 100) / 100;
        setReach((old) => (old && old.left === left && old.right === right ? old : { left, right }));
      }
      raf = window.requestAnimationFrame(tick);
    };
    raf = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(raf);
  }, [index]);
  return reach;
}

/** A dead zone per stick, for a worn stick that drifts: let go of the stick, see where it rests,
 *  and set the zone a little past that. */
function DeadzoneEditor({
  pad,
  initial,
  onCancel,
  onDone,
}: {
  pad: PadInfo;
  initial: StickDeadzones;
  onCancel: () => void;
  onDone: (dz: StickDeadzones) => void;
}) {
  const tr = useTr();
  const [dz, setDz] = useState<StickDeadzones>(initial);
  const reach = useStickReach(pad.index);
  const max = pct(MAX_STICK_DEADZONE);

  const row = (which: keyof StickDeadzones, label: string) => {
    const now = reach ? pct(reach[which]) : null;
    // A zone under the usual one plays as the usual one (remotePlayStream.ts).
    const inside = now !== null && now <= Math.max(pct(dz[which]), pct(USUAL_STICK_DEADZONE));
    return (
      <div className="space-y-1">
        <label className="flex items-center gap-3 text-xs">
          <span className="w-24 shrink-0">{label}</span>
          <input
            type="range"
            min={0}
            max={max}
            value={pct(dz[which])}
            onChange={(e) => setDz((d) => ({ ...d, [which]: Number(e.target.value) / 100 }))}
            className="min-w-0 flex-1 accent-[var(--color-accent)]"
            aria-label={label}
            data-testid={`rp-pad-deadzone-${which}`}
          />
          <span className="w-10 text-end tabular-nums">{pct(dz[which])}%</span>
        </label>
        {now !== null && (
          <p className={`text-xs ${inside ? "text-[var(--color-muted)]" : "text-[var(--color-warn)]"}`}>
            {inside
              ? tr("rp_pad_deadzone_now_in", { n: now }, `Now ${now}% out: inside the zone, the PS5 sees it centered.`)
              : tr("rp_pad_deadzone_now_out", { n: now }, `Now ${now}% out: past the zone, the PS5 sees it move.`)}
          </p>
        )}
      </div>
    );
  };

  return (
    <div className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-accent)] p-3" data-testid="rp-pad-deadzone-editor">
      <div className="text-xs text-[var(--color-muted)]">{padName(pad.id)}</div>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_pad_deadzone_hint",
          undefined,
          "For a stick that moves by itself (drift): let go of it, see how far out it rests, and set its zone a little past that. Pushed all the way still reaches the end. Under 8% is the usual small zone.",
        )}
      </p>
      {row("left", tr("rp_pad_deadzone_left", undefined, "Left stick"))}
      {row("right", tr("rp_pad_deadzone_right", undefined, "Right stick"))}
      {!reach && (
        <p className="text-xs text-[var(--color-warn)]">
          {tr("rp_pad_gone", undefined, "The controller went away. Press a button on it.")}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="primary" onClick={() => onDone(dz)} data-testid="rp-pad-deadzone-save">
          {tr("rp_pad_extra_save", undefined, "Save")}
        </Button>
        {(initial.left > 0 || initial.right > 0) && (
          <Button size="sm" variant="ghost" onClick={() => onDone({ left: 0, right: 0 })}>
            {tr("rp_pad_reset", undefined, "Reset")}
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {tr("rp_pad_cancel", undefined, "Cancel")}
        </Button>
      </div>
    </div>
  );
}
