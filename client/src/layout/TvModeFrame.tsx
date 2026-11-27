// TV mode's frame (#976, state/tvMode): on its own screens the shell shows no sidebar, tab bar
// or status bar, only the screen. The full app gets a bar back to TV mode while TV mode is
// wanted on this device.

import { useEffect, useRef } from "react";
import { useLocation, useNavigate } from "react-router";
import { Tv } from "lucide-react";

import { openModalCount, takeBackStep } from "../lib/focusTrap";
import { canLeaveApp } from "../lib/tvNav";
import { useTr } from "../state/lang";
import { isTvRoute, useTvMode, useTvModeStore, useTvModeWanted } from "../state/tvMode";

/** Whether a key press is Back in TV mode's frame: a plain Esc that nothing used, with no dialog
 *  open when it came (that closes on it by itself), and not on the game's picture (Esc is a
 *  button of the game there). */
export function escapeGoesBack(
  e: Pick<KeyboardEvent, "key" | "defaultPrevented" | "altKey" | "ctrlKey" | "metaKey" | "shiftKey" | "target">,
  dialogOpen: boolean,
): boolean {
  if (e.key !== "Escape" || e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return false;
  if (dialogOpen) return false;
  return !(e.target as Element | null)?.closest?.("[data-own-arrows]");
}

/** Whether the shell shows TV mode's plain frame now. Marks the page for the arrows
 *  (lib/tvNav installTvModeNav) and the TV styles (index.css). */
export function useTvFrame(): boolean {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const on = useTvMode();
  const lean = on && isTvRoute(pathname);
  const where = useRef(pathname);
  useEffect(() => {
    where.current = pathname;
  });
  useEffect(() => {
    const root = document.documentElement;
    if (lean) root.dataset.tvMode = "1";
    else delete root.dataset.tvMode;
    return () => {
      delete root.dataset.tvMode;
    };
  }, [lean]);
  // A keyboard's Esc is Back there, where there is no system Back to press (a computer, the web
  // UI): what is open on top first (it closes on Esc by itself), then the screen's own step.
  useEffect(() => {
    if (!lean || canLeaveApp()) return;
    // A dialog open when the key came closes on it by itself; by the time the key reaches the
    // window it may be gone already, so it is noted first (capture runs before anything else).
    const dialogKeys = new WeakSet<Event>();
    const onCapture = (e: KeyboardEvent) => {
      if (e.key === "Escape" && openModalCount() > 0) dialogKeys.add(e);
    };
    const onKey = (e: KeyboardEvent) => {
      if (!escapeGoesBack(e, dialogKeys.has(e) || openModalCount() > 0)) return;
      e.preventDefault();
      // No step of the screen's own: TV mode's home, the screen all others come from.
      if (!takeBackStep() && where.current !== "/tv") navigate("/tv", { replace: true });
    };
    window.addEventListener("keydown", onCapture, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onCapture, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [lean, navigate]);
  return lean;
}

/** In the full app while this device wants TV mode: one line, and the way back to it. */
export function TvModeReturnBar() {
  const tr = useTr();
  const navigate = useNavigate();
  const { pathname } = useLocation();
  const wanted = useTvModeWanted();
  const on = useTvMode();
  const setFullApp = useTvModeStore((s) => s.setFullApp);
  if (!wanted || (on && isTvRoute(pathname))) return null;
  return (
    <div
      className="mx-3 mt-2 flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] px-4 py-2 md:mx-6"
      data-testid="tv-return-bar"
    >
      <Tv size={18} className="shrink-0 text-[var(--color-accent)]" aria-hidden />
      <span className="min-w-0 flex-1 text-sm">
        {tr("tv_return_body", undefined, "This is the full app. TV mode has the console and Play on one screen.")}
      </span>
      <button
        type="button"
        onClick={() => {
          setFullApp(false);
          navigate("/tv");
        }}
        className="min-h-10 shrink-0 rounded-full bg-[var(--color-accent)] px-4 text-sm font-semibold text-[var(--color-accent-contrast)] hover:brightness-110"
        data-testid="tv-return"
      >
        {tr("tv_return", undefined, "Back to TV mode")}
      </button>
    </div>
  );
}
