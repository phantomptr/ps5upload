import { useEffect, useState } from "react";
import { Link } from "react-router";
import { Eraser } from "lucide-react";

import { fetchVolumes, type Volume } from "../../api/ps5";
import { Card } from "../../components";
import { formatBytes } from "../../lib/format";
import { useTr } from "../../state/lang";

/** Below this share of a drive free, or this much, Home suggests Clean up. */
const LOW_SHARE = 0.1;
const LOW_BYTES = 20 * 1024 ** 3;

/** The fullest real drive that is low on room, or null when none is. */
export function lowDrive(volumes: readonly Volume[]): Volume | null {
  let worst: Volume | null = null;
  for (const v of volumes) {
    if (v.is_placeholder || !v.writable || v.total_bytes <= 0) continue;
    const low = v.free_bytes < LOW_BYTES || v.free_bytes / v.total_bytes < LOW_SHARE;
    if (low && (!worst || v.free_bytes / v.total_bytes < worst.free_bytes / worst.total_bytes)) worst = v;
  }
  return worst;
}

/** A nudge to Clean up when one of the console's drives is nearly full. Reads the drives once
 *  per console connection; draws nothing otherwise. */
export function LowSpaceCard({ host }: { host: string }) {
  const tr = useTr();
  const [low, setLow] = useState<Volume | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLow(null);
    fetchVolumes(host)
      .then((v) => {
        if (!cancelled) setLow(lowDrive(v));
      })
      .catch(() => {
        // Nothing to say without the drive list.
      });
    return () => {
      cancelled = true;
    };
  }, [host]);

  if (!low) return null;
  return (
    <Card>
      <div className="flex flex-wrap items-center gap-3" data-testid="home-low-space">
        <Eraser size={18} className="shrink-0 text-[var(--color-warn)]" />
        <div className="min-w-0 flex-1 basis-[12rem] text-sm">
          <div className="font-medium">
            {tr("home_low_space_title", { drive: low.path, free: formatBytes(low.free_bytes) }, `${low.path} has ${formatBytes(low.free_bytes)} free`)}
          </div>
          <div className="text-xs text-[var(--color-muted)]">
            {tr(
              "home_low_space_body",
              undefined,
              "Clean up finds updates, leftover packages and games you never play that can go.",
            )}
          </div>
        </div>
        <Link
          to="/cleanup"
          className="inline-flex items-center rounded-[var(--radius-card)] border border-[var(--color-border)] px-3 py-1.5 text-xs font-medium hover:bg-[var(--color-surface-3)] max-md:min-h-11"
        >
          {tr("cw_cleanup_title", undefined, "Clean up")}
        </Link>
      </div>
    </Card>
  );
}
