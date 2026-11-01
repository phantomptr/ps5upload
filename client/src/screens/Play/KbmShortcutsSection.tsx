import { useEffect, useState } from "react";

import { Button, Toggle } from "../../components";
import { useTr } from "../../state/lang";
import { useKbmStore } from "../../state/kbm";
import { useRemotePlayStore } from "../../state/remotePlay";
import {
  DEFAULT_SHORTCUTS,
  SHORTCUT_PROBLEM_TEXT,
  ShortcutRecorder,
  sameShortcut,
  shortcutProblem,
  type ShortcutKind,
  type ShortcutProblem,
} from "../../lib/kbmShortcuts";
import { KeyCaps } from "./KbmHints";

/** Settings › Controls › Shortcuts: the keys that give the keyboard and mouse back and switch the
 *  input mode (lib/kbmShortcuts.ts), recorded live, checked against the app's shortcuts, the
 *  keyboard controls and each other; and how the release key is shown. */
export function KbmShortcutsSection() {
  const tr = useTr();
  const shortcuts = useKbmStore((s) => s.shortcuts);
  const setShortcut = useKbmStore((s) => s.setShortcut);
  const keymap = useRemotePlayStore((s) => s.keymap);
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  const [recording, setRecording] = useState<ShortcutKind | null>(null);
  const [problem, setProblem] = useState<{ kind: ShortcutKind; why: ShortcutProblem } | null>(null);

  useEffect(() => {
    if (!recording) return;
    const rec = new ShortcutRecorder();
    const other: ShortcutKind = recording === "release" ? "cycle" : "release";
    const on = (e: KeyboardEvent, down: boolean) => {
      // Every key is the recording's: no app shortcut, no Settings Esc, no game.
      e.preventDefault();
      e.stopPropagation();
      if (down && e.repeat) return;
      const r = rec.feed(e, down);
      if (!r.done) return;
      setRecording(null);
      if ("cancelled" in r) return;
      const why = shortcutProblem(recording, r.shortcut, { other: shortcuts[other], keymap });
      if (why) {
        setProblem({ kind: recording, why });
        return;
      }
      setProblem(null);
      setShortcut(recording, r.shortcut);
    };
    const kd = (e: KeyboardEvent) => on(e, true);
    const ku = (e: KeyboardEvent) => on(e, false);
    // On the window, capturing: before the drawer's own Esc and focus handling.
    window.addEventListener("keydown", kd, true);
    window.addEventListener("keyup", ku, true);
    return () => {
      window.removeEventListener("keydown", kd, true);
      window.removeEventListener("keyup", ku, true);
    };
  }, [recording, shortcuts, keymap, setShortcut]);

  const row = (kind: ShortcutKind, label: string) => {
    const isDefault = sameShortcut(shortcuts[kind], DEFAULT_SHORTCUTS[kind]);
    const listening = recording === kind;
    return (
      <div className="space-y-1" data-testid={`rp-sc-${kind}`}>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-sm">{label}</span>
          <span className="flex items-center gap-2">
            {listening ? (
              <span className="text-xs text-[var(--color-accent)]" data-testid={`rp-sc-${kind}-listening`}>
                {tr("rp_sc_press", undefined, "Press the keys… (Esc cancels)")}
              </span>
            ) : (
              <span className="rounded-lg bg-black/70 px-1.5 py-1" data-testid={`rp-sc-${kind}-keys`}>
                <KeyCaps shortcut={shortcuts[kind]} size="sm" />
              </span>
            )}
            <Button
              size="sm"
              variant="ghost"
              onClick={() => {
                setProblem(null);
                setRecording(listening ? null : kind);
              }}
              data-testid={`rp-sc-${kind}-change`}
            >
              {listening ? tr("rp_sc_cancel", undefined, "Cancel") : tr("rp_sc_change", undefined, "Change")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={isDefault}
              onClick={() => {
                setProblem(null);
                setRecording(null);
                setShortcut(kind, null);
              }}
              data-testid={`rp-sc-${kind}-reset`}
            >
              {tr("rp_sc_reset", undefined, "Reset")}
            </Button>
          </span>
        </div>
        {problem?.kind === kind && (
          <p className="text-xs text-[var(--color-danger,#dc2626)]" role="alert" data-testid={`rp-sc-${kind}-problem`}>
            {tr(SHORTCUT_PROBLEM_TEXT[problem.why].key, undefined, SHORTCUT_PROBLEM_TEXT[problem.why].en)}
          </p>
        )}
      </div>
    );
  };

  return (
    <section className="space-y-3" data-testid="rp-shortcuts">
      <h3 className="text-sm font-semibold">{tr("rp_sc_title", undefined, "Shortcuts")}</h3>
      {row("release", tr("rp_sc_release", undefined, "Release keyboard & mouse"))}
      {row("cycle", tr("rp_sc_cycle", undefined, "Switch input mode"))}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_sc_hint",
          undefined,
          "Modifiers alone (like Ctrl and Alt) work when pressed and let go with no other key. With another key, they still reach the PS5.",
        )}
      </p>
      <Toggle
        checked={prefs.kbmChip}
        onChange={(v) => setPrefs({ kbmChip: v })}
        label={tr("rp_sc_chip", undefined, "Show the release key while playing")}
      />
      <Toggle
        checked={prefs.escHoldRelease}
        onChange={(v) => setPrefs({ escHoldRelease: v })}
        label={tr("rp_sc_esc_hold", undefined, "Hold Esc to release too")}
      />
    </section>
  );
}
