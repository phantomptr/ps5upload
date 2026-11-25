import { useEffect, useRef } from "react";
import { useNavigate } from "react-router";
import { LogIn, Users } from "lucide-react";

import { Button } from "../../components";
import { tvNavActive } from "../../lib/tvNav";
import { useTr } from "../../state/lang";

/**
 * On a TV only (#926): the app's first screen offers joining a friend's game, the thing a TV is
 * mostly used for, with a big button that takes the remote's focus. It opens Play › Join a friend,
 * where the link or the short code is typed. Nothing changes anywhere else.
 */
export function TvJoinCard({ className = "" }: { className?: string }) {
  const tr = useTr();
  const navigate = useNavigate();
  const tv = tvNavActive();
  const box = useRef<HTMLElement>(null);
  useEffect(() => {
    if (!tv) return;
    // Not away from what the remote is already on.
    const a = document.activeElement;
    if (a && a !== document.body) return;
    box.current
      ?.querySelector<HTMLElement>('[data-testid="tv-join-go"]')
      ?.focus({ preventScroll: true });
  }, [tv]);
  if (!tv) return null;
  return (
    <section
      ref={box}
      className={`flex flex-col gap-4 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-6 sm:flex-row sm:items-center ${className}`}
      data-testid="tv-join"
    >
      <span className="grid h-14 w-14 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
        <Users size={28} aria-hidden />
      </span>
      <div className="min-w-0 flex-1">
        <h2 className="text-xl font-semibold">
          {tr("pt_title", undefined, "Play Together")}
        </h2>
        <p className="mt-1 text-sm text-[var(--color-muted)]">
          {tr(
            "tv_join_body",
            undefined,
            "Got a link or a code from a friend? Play their PS5 on this TV.",
          )}
        </p>
      </div>
      <Button
        variant="primary"
        size="lg"
        leftIcon={<LogIn size={18} />}
        onClick={() => navigate("/play?mode=friend")}
        data-testid="tv-join-go"
      >
        {tr("tv_join_button", undefined, "Join Play Together")}
      </Button>
    </section>
  );
}
