import { useEffect, useRef, type ReactNode } from "react";

import type { PmOverview } from "../../../api/payloadManager";
import { mgmtAddr } from "../../../lib/addr";
import { useMounterFor } from "../../../lib/mounter";
import { useTr } from "../../../state/lang";
import type { IntegrationReport } from "./registry";

/** Payload Manager's card line: how many payloads it keeps and, once "On your PS5" has matched
 *  them to what runs, how many of those are running (they show on its rows, not as cards of
 *  their own, so the line says so with the card closed). The overview is read by the panel. */
export function PmSummary({
  view,
  running,
  onReport,
  fallback,
}: {
  view: PmOverview | null | undefined;
  /** Saved payloads running now; null while that isn't known. */
  running: number | null;
  onReport: (r: IntegrationReport) => void;
  fallback: ReactNode;
}) {
  const tr = useTr();
  const reportRef = useRef(onReport);
  useEffect(() => {
    reportRef.current = onReport;
  });
  const version = view?.present ? view.version || null : undefined;
  useEffect(() => {
    if (version !== undefined) reportRef.current({ version });
  }, [version]);
  // A list that couldn't be read is not an empty one ("0 saved").
  if (!view?.present || view.payloadsError) return <>{fallback}</>;
  const n = view.payloads.length;
  if (running === null) return <>{tr("hub_pm_summary", { n }, `Payloads stored: ${n}`)}</>;
  return <>{tr("hub_pm_summary_running", { n, running }, `${n} saved · ${running} running`)}</>;
}

/** ShadowMount+'s card line: how many game images it has mounted, while it runs. Reads the
 *  mounter snapshot the panel takes on each refresh. */
export function SmpSummary({
  host,
  running,
  fallback,
}: {
  host: string;
  running: boolean;
  fallback: ReactNode;
}) {
  const tr = useTr();
  const snapshot = useMounterFor(host.trim() ? mgmtAddr(host.trim()) : null);
  const mounted = snapshot ? snapshot.mounted_images.length : null;
  if (!running || mounted === null) return <>{fallback}</>;
  return <>{tr("hub_smp_summary", { n: mounted }, `Game images mounted: ${mounted}`)}</>;
}
