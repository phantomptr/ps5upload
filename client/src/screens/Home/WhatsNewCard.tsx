import { useEffect, useState } from "react";
import { Link } from "react-router";
import { Sparkles, X } from "lucide-react";

import { Card } from "../../components";
import { getAppVersion } from "../../lib/appVersion";
import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { useTr } from "../../state/lang";

const KEY = "ps5upload.whats_new_seen.v1";
/** How long after an update Home mentions it, unless dismissed first. */
const SHOW_MS = 7 * 24 * 3600 * 1000;

type Seen = { version: string; since: number; dismissed?: boolean };

function load(): Seen | null {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "null") as Seen | null;
    return v && typeof v.version === "string" ? v : null;
  } catch {
    return null;
  }
}

/** Whether Home should say the app was updated, and what to remember now. A first run is not an
 *  update: it remembers the version and says nothing. */
export function whatsNewState(seen: Seen | null, version: string, now: number): { show: boolean; next: Seen } {
  if (!seen) return { show: false, next: { version, since: now, dismissed: true } };
  if (seen.version !== version) return { show: true, next: { version, since: now } };
  return { show: !seen.dismissed && now - seen.since < SHOW_MS, next: seen };
}

/** What's new is in Help, not the sidebar: for a few days after an update Home points at it. */
export function WhatsNewCard() {
  const tr = useTr();
  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getAppVersion()
      .then((v) => {
        if (cancelled) return;
        const { show, next } = whatsNewState(load(), v, Date.now());
        safeSetItem(KEY, JSON.stringify(next));
        // Also cleared: dismissed on another console's Home, this one is shown again without it.
        setVersion(show ? v : null);
      })
      .catch(() => {
        // No version, nothing to announce.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (!version) return null;
  const dismiss = () => {
    safeSetItem(KEY, JSON.stringify({ version, since: Date.now(), dismissed: true }));
    setVersion(null);
  };
  return (
    <Card>
      <div className="flex flex-wrap items-center gap-3" data-testid="home-whats-new">
        <Sparkles size={18} className="shrink-0 text-[var(--color-accent)]" />
        <div className="min-w-[8rem] flex-1 text-sm font-medium">
          {tr("home_updated_to", { version }, `Updated to v${version}`)}
        </div>
        <Link
          to="/faq?tab=whats-new"
          onClick={dismiss}
          className="inline-flex items-center rounded-[var(--radius-card)] border border-[var(--color-border)] px-3 py-1.5 text-xs font-medium hover:bg-[var(--color-surface-3)] max-md:min-h-11"
        >
          {tr("home_see_whats_new", undefined, "See what's new")}
        </Link>
        <button
          type="button"
          onClick={dismiss}
          aria-label={tr("dismiss", undefined, "Dismiss")}
          className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] max-md:min-h-11 max-md:min-w-11"
        >
          <X size={14} />
        </button>
      </div>
      {headline13(version) && (
        <div className="mt-3 space-y-2 border-t border-[var(--color-border)] pt-3 text-sm" data-testid="home-whats-new-13">
          <p className="font-medium">{tr("whats_new_13_title", undefined, "New in 13.0.0: Play Rooms")}</p>
          <p className="text-[var(--color-muted)]">
            {tr(
              "whats_new_13_body",
              undefined,
              "Create a room for your PS5 games, make it private, for friends or public, and find rooms to join. Chat and talk while you play. A friend with a PS5 can join on their TV with PS5Upload Play.",
            )}
          </p>
          <div className="flex flex-wrap gap-2">
            <Link
              to="/play?mode=rooms"
              onClick={dismiss}
              className="inline-flex items-center rounded-[var(--radius-card)] bg-[var(--color-accent)] px-3 py-1.5 text-xs font-medium text-[var(--color-on-accent,#fff)] max-md:min-h-11"
              data-testid="home-whats-new-rooms"
            >
              {tr("whats_new_13_open", undefined, "Open Play Room")}
            </Link>
            <Link
              to="/bug-report"
              onClick={dismiss}
              className="inline-flex items-center rounded-[var(--radius-card)] border border-[var(--color-border)] px-3 py-1.5 text-xs font-medium hover:bg-[var(--color-surface-3)] max-md:min-h-11"
              data-testid="home-whats-new-report"
            >
              {tr("whats_new_13_report", undefined, "Send a bug report")}
            </Link>
          </div>
          <p className="text-xs text-[var(--color-muted)]" data-testid="home-whats-new-test">
            {tr(
              "whats_new_13_test",
              undefined,
              "Help us test: Play Rooms, PS5Upload Play and the new things on the PS5 (Minion's queue, save sync, freeing space while extracting, the USB adapter) have been tried on only a few consoles. If something goes wrong, open Bug Report, save the zip and post it on GitHub or Discord.",
            )}
          </p>
        </div>
      )}
    </Card>
  );
}

/** The 13.0.0 headline (Play Rooms) shows with the "Updated to" card of any 13.x, once. */
export function headline13(version: string): boolean {
  return /^13\./.test(version);
}
