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
        if (show) setVersion(v);
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
    </Card>
  );
}
