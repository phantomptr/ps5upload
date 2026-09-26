import { ArrowLeft, ArrowRight } from "lucide-react";
import { useEffect, useState } from "react";
import {
  useLocation,
  useNavigate,
  useNavigationType,
} from "react-router";

import { useTr } from "../state/lang";

function browserHistoryIndex(): number | null {
  const value = window.history.state?.idx;
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

/** Browser-style Back and Forward for the desktop app shell.
 *
 * React Router stores a monotonic `idx` in each browser-history entry. Keeping
 * the greatest reachable index lets us disable Forward correctly after a POP;
 * a fresh PUSH truncates the old forward branch, just like a browser.
 */
export default function NavigationControls() {
  const tr = useTr();
  const navigate = useNavigate();
  const location = useLocation();
  const navigationType = useNavigationType();
  const [position, setPosition] = useState(() => browserHistoryIndex() ?? 0);
  const [furthest, setFurthest] = useState(() => browserHistoryIndex() ?? 0);

  useEffect(() => {
    const index = browserHistoryIndex();
    if (index === null) return;
    setPosition(index);
    setFurthest((previous) =>
      navigationType === "PUSH" ? index : Math.max(previous, index),
    );
  }, [location.key, navigationType]);

  const backLabel = tr("navigation_back", undefined, "Back");
  const forwardLabel = tr("navigation_forward", undefined, "Forward");

  return (
    <nav
      aria-label={tr("navigation_history", undefined, "View history")}
      className="flex items-center gap-1"
    >
      <button
        type="button"
        aria-label={backLabel}
        title={backLabel}
        disabled={position <= 0}
        onClick={() => navigate(-1)}
        className="inline-flex h-8 w-8 items-center justify-center rounded-md text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-35"
      >
        <ArrowLeft size={16} />
      </button>
      <button
        type="button"
        aria-label={forwardLabel}
        title={forwardLabel}
        disabled={position >= furthest}
        onClick={() => navigate(1)}
        className="inline-flex h-8 w-8 items-center justify-center rounded-md text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] disabled:cursor-not-allowed disabled:opacity-35"
      >
        <ArrowRight size={16} />
      </button>
    </nav>
  );
}
