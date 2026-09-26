import { useCallback, useRef, useState, type ReactNode } from "react";
import { RefreshCw } from "lucide-react";

import { haptic } from "../lib/haptics";
import { Spinner } from "./Spinner";

/** Pull-to-refresh wrapper for touch lists (v5 §6.3 mobile).
 *
 * The browser's pull-to-refresh is killed globally (`overscroll-behavior-y:
 * contain` on body). This component provides the app's OWN pull-to-refresh
 * for lists that want it (Library, Tasks, Notifications).
 *
 * Listens to touchstart/touchmove/touchend on the wrapper. When the user
 * drags down past `threshold` (64px) while scrolled to top, fires
 * `onRefresh()` — which should return a Promise that resolves when the
 * refresh is done. Shows a spinner during refresh, an arrow before.
 *
 * Only activates on touch devices (pointer: coarse). On mouse/keyboard,
 * renders children as-is with no gesture handling.
 *
 * The wrapper is a div with `overflow-y-auto` — put it where the
 * scrollable list would go. Children are the list content. */

const THRESHOLD = 64;
const MAX_PULL = 96;

export function PullToRefresh({
  onRefresh,
  children,
  className = "",
  ariaLabel,
}: {
  onRefresh: () => Promise<void>;
  children: ReactNode;
  className?: string;
  ariaLabel?: string;
}): ReactNode {
  const [pullDistance, setPullDistance] = useState(0);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [isDragging, setIsDragging] = useState(false);
  const startYRef = useRef<number | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  const isTouch = useCallback(() => {
    if (typeof window === "undefined") return false;
    return window.matchMedia("(pointer: coarse)").matches;
  }, []);

  const onTouchStart = useCallback(
    (e: React.TouchEvent) => {
      if (!isTouch() || isRefreshing) return;
      const el = containerRef.current;
      if (!el || el.scrollTop > 0) return;
      startYRef.current = e.touches[0]?.clientY ?? null;
      if (startYRef.current !== null) setIsDragging(true);
    },
    [isTouch, isRefreshing],
  );

  const onTouchMove = useCallback(
    (e: React.TouchEvent) => {
      if (startYRef.current === null || isRefreshing) return;
      const el = containerRef.current;
      if (!el || el.scrollTop > 0) {
        startYRef.current = null;
        setIsDragging(false);
        setPullDistance(0);
        return;
      }
      const delta = (e.touches[0]?.clientY ?? 0) - startYRef.current;
      if (delta <= 0) {
        setPullDistance(0);
        return;
      }
      // Rubber-band: resistance increases with pull distance so it feels
      // progressively harder to drag, like a spring.
      const resisted = Math.min(MAX_PULL, delta * 0.5);
      setPullDistance(resisted);
    },
    [isRefreshing],
  );

  const onTouchEnd = useCallback(async () => {
    if (startYRef.current === null) return;
    startYRef.current = null;
    setIsDragging(false);
    if (pullDistance >= THRESHOLD && !isRefreshing) {
      haptic("confirm");
      setIsRefreshing(true);
      setPullDistance(THRESHOLD);
      try {
        await onRefresh();
      } finally {
        setIsRefreshing(false);
        setPullDistance(0);
      }
    } else {
      setPullDistance(0);
    }
  }, [pullDistance, isRefreshing, onRefresh]);

  // Non-touch: render a plain scroll container, no gesture overhead.
  if (!isTouch()) {
    return (
      <div className={`overflow-y-auto ${className}`} ref={containerRef}>
        {children}
      </div>
    );
  }

  const showSpinner = isRefreshing || pullDistance >= THRESHOLD;
  const pct = Math.min(1, pullDistance / THRESHOLD);

  return (
    <div
      ref={containerRef}
      className={`relative overflow-y-auto [overscroll-behavior:contain] ${className}`}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      role="region"
      aria-label={ariaLabel}
    >
      {/* Pull indicator — sits above the list, translated down with the pull. */}
      {(pullDistance > 0 || isRefreshing) && (
        <div
          className="pointer-events-none absolute left-0 right-0 top-0 z-10 flex items-center justify-center"
          style={{ height: `${pullDistance}px` }}
        >
          {showSpinner ? (
            <span style={{ opacity: isRefreshing ? 1 : pct }}>
              <Spinner size={24} tone="accent" />
            </span>
          ) : (
            <RefreshCw
              size={22}
              className="text-[var(--color-muted)] transition-opacity"
              style={{
                opacity: pct,
                transform: `rotate(${pct * 180}deg)`,
              }}
            />
          )}
        </div>
      )}
      {/* The content translates down with the pull; snaps back via transition
       * when released. During refresh, it stays pinned at THRESHOLD. */}
      <div
        style={{
          transform: `translateY(${pullDistance}px)`,
          transition: !isDragging && !isRefreshing ? "transform 0.2s ease-out" : "none",
        }}
      >
        {children}
      </div>
    </div>
  );
}
