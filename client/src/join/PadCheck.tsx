// "Check your controller" on the Play Together guest page: which controller this browser sees,
// whether it is recognized, a live view where the buttons pressed light up, and a quick mapping
// (press the bottom button, the right one... then the sticks) saved on this device for that
// controller (lib/padProfiles.ts, keyed by Gamepad.id). Any controller can be checked from the
// page's buttons; one the browser and the known controllers' table do not recognize is offered it.

import { useEffect, useRef, useState } from "react";
import { Gamepad2, X } from "lucide-react";

import { gamepadsOf } from "../lib/nativePad";
import { isWeak8BitDo, padName } from "../lib/padMappings";
import {
  CHECK_STEPS,
  defaultProfileFor,
  loadProfiles,
  PadLearner,
  padKind,
  profileFor,
  saveProfile,
  snapshot,
  type MapStep,
} from "../lib/padProfiles";
import { IDLE_PAD, PadButton, padFromGamepads, type PadState } from "../lib/remotePlayStream";
import { EightBitDoHint } from "../screens/Play/PadHint";
import { useTr } from "../state/lang";
import { setPadChecking } from "./session";

type Tr = ReturnType<typeof useTr>;

const connectedPads = (): Gamepad[] =>
  gamepadsOf(typeof navigator === "undefined" ? undefined : navigator).filter((g): g is Gamepad => !!g?.connected);

/** The step's words: the face buttons by where they are, so a Nintendo-layout pad's bottom
 *  button (B on it) is ✕. */
export function checkStepText(tr: Tr, s: MapStep): string {
  if (s.kind === "axis") {
    if (s.which === "lx") return tr("rp_pad_step_lx", undefined, "Push the left stick right");
    if (s.which === "ly") return tr("rp_pad_step_ly", undefined, "Push the left stick down");
    if (s.which === "rx") return tr("rp_pad_step_rx", undefined, "Push the right stick right");
    return tr("rp_pad_step_ry", undefined, "Push the right stick down");
  }
  if (s.kind === "button") {
    if (s.bit === PadButton.CROSS) return tr("join_check_bottom", undefined, "Press the bottom face button (✕)");
    if (s.bit === PadButton.CIRCLE) return tr("join_check_right", undefined, "Press the right face button (○)");
    if (s.bit === PadButton.SQUARE) return tr("join_check_left", undefined, "Press the left face button (□)");
    if (s.bit === PadButton.TRIANGLE) return tr("join_check_top", undefined, "Press the top face button (△)");
  }
  return tr("rp_pad_press", { name: s.name }, `Press ${s.name}`);
}

/** The controller at `index` read every frame while mounted. */
function useFrames(index: number | null, cb: (g: Gamepad | null) => void) {
  const ref = useRef(cb);
  useEffect(() => {
    ref.current = cb;
  });
  useEffect(() => {
    if (index === null) return;
    let raf = 0;
    const tick = () => {
      ref.current(connectedPads().find((g) => g.index === index) ?? null);
      raf = window.requestAnimationFrame(tick);
    };
    raf = window.requestAnimationFrame(tick);
    return () => window.cancelAnimationFrame(raf);
  }, [index]);
}

const TEST_BUTTONS: Array<[number, string]> = [
  [PadButton.CROSS, "✕"],
  [PadButton.CIRCLE, "○"],
  [PadButton.SQUARE, "□"],
  [PadButton.TRIANGLE, "△"],
  [PadButton.UP, "↑"],
  [PadButton.DOWN, "↓"],
  [PadButton.LEFT, "←"],
  [PadButton.RIGHT, "→"],
  [PadButton.L1, "L1"],
  [PadButton.R1, "R1"],
  [PadButton.L3, "L3"],
  [PadButton.R3, "R3"],
  [PadButton.SHARE, "Create"],
  [PadButton.OPTIONS, "Options"],
  [PadButton.PS, "PS"],
  [PadButton.TOUCHPAD, "Touchpad"],
];

/** The PS5 pad this controller makes right now: what is pressed lights up. */
export function PadTest({ pad }: { pad: PadState }) {
  const chip = (on: boolean) =>
    `flex h-8 min-w-8 items-center justify-center rounded-md px-1.5 text-xs font-medium ${
      on ? "bg-white text-black" : "bg-white/10 text-white/70"
    }`;
  const stick = (x: number, y: number, label: string) => (
    <span className="relative flex h-12 w-12 items-center justify-center rounded-full bg-white/10 text-[10px] text-white/50">
      {label}
      <span
        className="absolute h-3 w-3 rounded-full bg-white"
        style={{ left: `calc(50% + ${(x / 32767) * 18}px - 6px)`, top: `calc(50% + ${(y / 32767) * 18}px - 6px)` }}
      />
    </span>
  );
  return (
    <div className="space-y-2" data-testid="join-pad-test" data-buttons={pad.buttons}>
      <div className="flex flex-wrap gap-1">
        {TEST_BUTTONS.map(([bit, name]) => (
          <span key={bit} className={chip((pad.buttons & bit) !== 0)} data-on={(pad.buttons & bit) !== 0 ? "1" : "0"}>
            {name}
          </span>
        ))}
        <span className={chip(pad.l2 > 30)}>L2</span>
        <span className={chip(pad.r2 > 30)}>R2</span>
      </div>
      <div className="flex gap-3">
        {stick(pad.lx, pad.ly, "L")}
        {stick(pad.rx, pad.ry, "R")}
      </div>
    </div>
  );
}

/** The check, over the game: the game gets no input from this device while it is open. */
export function PadCheck({ onClose }: { onClose: () => void }) {
  const tr = useTr();
  const [pads, setPads] = useState<Array<{ index: number; id: string }>>(() =>
    connectedPads().map((g) => ({ index: g.index, id: g.id })),
  );
  const [index, setIndex] = useState<number | null>(() => connectedPads()[0]?.index ?? null);
  const [live, setLive] = useState<PadState>(IDLE_PAD);
  const [learning, setLearning] = useState<PadLearner | null>(null);
  const [step, setStep] = useState(0);
  const [saved, setSaved] = useState(loadProfiles);
  const [notice, setNotice] = useState<string | null>(null);
  const [current, setCurrent] = useState<Gamepad | null>(null);

  useEffect(() => {
    setPadChecking(true);
    return () => setPadChecking(false);
  }, []);

  // Controllers come and go (a browser shows one only after a press on it).
  useEffect(() => {
    const update = () => {
      const now = connectedPads();
      setPads((old) =>
        old.length === now.length && old.every((p, i) => p.id === now[i].id && p.index === now[i].index)
          ? old
          : now.map((g) => ({ index: g.index, id: g.id })),
      );
      setIndex((i) => (i !== null && now.some((g) => g.index === i) ? i : (now[0]?.index ?? null)));
    };
    const t = window.setInterval(update, 500);
    window.addEventListener("gamepadconnected", update);
    window.addEventListener("gamepaddisconnected", update);
    return () => {
      window.clearInterval(t);
      window.removeEventListener("gamepadconnected", update);
      window.removeEventListener("gamepaddisconnected", update);
    };
  }, []);

  useFrames(index, (g) => {
    setCurrent((c) => (c?.id === g?.id ? c : g));
    if (!g) return;
    if (learning) {
      if (learning.feed(snapshot(g))) finishOrNext(learning, g);
      return;
    }
    const p = padFromGamepads([g], profileFor);
    setLive((old) =>
      old.buttons === p.buttons && old.l2 === p.l2 && old.r2 === p.r2 && old.lx === p.lx && old.ly === p.ly && old.rx === p.rx && old.ry === p.ry
        ? old
        : p,
    );
  });

  const finishOrNext = (l: PadLearner, g: Gamepad | null) => {
    if (!l.done) return setStep(l.step);
    const id = g?.id ?? current?.id;
    setLearning(null);
    if (!id) return;
    setSaved(saveProfile(id, l.profile(g ? defaultProfileFor(g) : undefined)));
    setNotice(tr("join_check_saved", undefined, "Saved for this controller on this device. Try it below."));
  };

  const start = () => {
    setNotice(null);
    setStep(0);
    setLearning(new PadLearner(CHECK_STEPS));
  };

  const g = current;
  const kind = g ? padKind(g) : null;
  const isSaved = !!(g && saved[g.id]);

  return (
    <div
      className="absolute inset-0 z-40 flex items-center justify-center bg-black/70 p-4 backdrop-blur-sm"
      data-testid="join-pad-check"
      onPointerDown={(e) => e.stopPropagation()}
    >
      <div className="w-[min(26rem,100%)] space-y-3 rounded-2xl border border-white/10 bg-neutral-900/95 p-4 text-sm text-white shadow-2xl">
        <div className="flex items-center gap-2">
          <Gamepad2 size={16} className="text-white/70" />
          <p className="flex-1 font-semibold">{tr("join_check_title", undefined, "Check your controller")}</p>
          <button
            type="button"
            onClick={onClose}
            aria-label={tr("join_done", undefined, "Done")}
            className="flex h-8 w-8 items-center justify-center rounded-full bg-white/10 hover:bg-white/20"
            data-testid="join-pad-check-close"
          >
            <X size={15} />
          </button>
        </div>

        {!g ? (
          <p className="text-white/70">{tr("rp_pads_none", undefined, "No controller seen yet. Connect one and press any button on it.")}</p>
        ) : (
          <>
            {pads.length > 1 && (
              <div className="flex flex-wrap gap-1">
                {pads.map((p) => (
                  <button
                    key={`${p.index}-${p.id}`}
                    type="button"
                    disabled={!!learning}
                    onClick={() => setIndex(p.index)}
                    className={`rounded-full px-2.5 py-1 text-xs ${p.index === index ? "bg-white text-black" : "bg-white/10"}`}
                  >
                    {padName(p.id)}
                  </button>
                ))}
              </div>
            )}
            <div>
              <p className="truncate font-medium" data-testid="join-pad-check-name">
                {padName(g.id)}
              </p>
              <p className="text-xs text-white/60" data-testid="join-pad-check-kind" data-kind={kind ?? ""}>
                {kind === "saved"
                  ? tr("rp_pad_mapped", undefined, "Plays with your mapping")
                  : kind === "guess"
                    ? tr(
                        "join_check_unknown",
                        undefined,
                        "This controller isn't on our list, so its buttons may be mixed up. Setting it up takes a few seconds.",
                      )
                    : tr("rp_pad_standard", undefined, "Recognized")}
              </p>
              {isWeak8BitDo(g) && <EightBitDoHint id={g.id} className="mt-1 block text-xs text-amber-200/90" />}
            </div>

            {learning ? (
              <div className="space-y-3" data-testid="join-pad-learn">
                <p className="text-xs text-white/60">
                  {tr("rp_pad_progress", { n: step + 1, total: learning.steps.length }, `${step + 1} of ${learning.steps.length}`)}
                </p>
                <p className="text-lg font-semibold" data-testid="join-pad-learn-step">
                  {learning.current ? checkStepText(tr, learning.current) : ""}
                </p>
                <div className="flex flex-wrap gap-2">
                  <button
                    type="button"
                    onClick={() => {
                      learning.skip();
                      finishOrNext(learning, g);
                    }}
                    className="rounded-full bg-white/15 px-3 py-1.5 text-xs hover:bg-white/25"
                    data-testid="join-pad-learn-skip"
                  >
                    {tr("rp_pad_skip", undefined, "Skip")}
                  </button>
                  {step > 0 && (
                    <button
                      type="button"
                      onClick={() => {
                        learning.back();
                        setStep(learning.step);
                      }}
                      className="rounded-full bg-white/15 px-3 py-1.5 text-xs hover:bg-white/25"
                    >
                      {tr("rp_pad_back", undefined, "Back")}
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={() => setLearning(null)}
                    className="rounded-full px-3 py-1.5 text-xs text-white/70 hover:bg-white/10"
                  >
                    {tr("rp_pad_cancel", undefined, "Cancel")}
                  </button>
                </div>
              </div>
            ) : (
              <>
                <p className="text-xs text-white/60">
                  {tr("join_check_try", undefined, "Press buttons and move the sticks: what you press lights up.")}
                </p>
                <PadTest pad={live} />
                {notice && (
                  <p className="text-xs text-emerald-300" role="status">
                    {notice}
                  </p>
                )}
                <div className="flex flex-wrap gap-2">
                  <button
                    type="button"
                    onClick={start}
                    className="rounded-full bg-white px-3 py-1.5 text-xs font-medium text-black"
                    data-testid="join-pad-learn-start"
                  >
                    {isSaved ? tr("join_check_redo", undefined, "Redo") : tr("join_check_map", undefined, "Set up the buttons")}
                  </button>
                  {isSaved && (
                    <button
                      type="button"
                      onClick={() => {
                        setSaved(saveProfile(g.id, null));
                        setNotice(null);
                      }}
                      className="rounded-full bg-white/15 px-3 py-1.5 text-xs hover:bg-white/25"
                      data-testid="join-pad-reset"
                    >
                      {tr("rp_pad_reset", undefined, "Reset")}
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={onClose}
                    className="rounded-full bg-white/15 px-3 py-1.5 text-xs hover:bg-white/25"
                    data-testid="join-pad-check-done"
                  >
                    {tr("join_done", undefined, "Done")}
                  </button>
                </div>
              </>
            )}
          </>
        )}
      </div>
    </div>
  );
}
