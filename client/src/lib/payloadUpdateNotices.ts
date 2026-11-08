// A small notice when payload updates are found and when one is applied. The engine checks (on
// the button, or daily with "Keep payloads up to date"); its state carries two counters that only
// grow, and this app remembers the last ones it told the user about.

import type { AppliedPayloadUpdate, PayloadUpdateState } from "../api/payloadUpdates";
import { payloadUpdatesOverview } from "../api/payloadUpdates";
import { safeGetItem, safeSetItem } from "./safeStorage";
import { trStatic } from "./trStatic";
import { pushNotification } from "../state/notifications";

export interface UpdateSeen {
  available_seq: number;
  applied_seq: number;
}

export type PayloadUpdateNotice =
  | { kind: "available"; names: string[] }
  | { kind: "applied"; items: AppliedPayloadUpdate[] };

const KEY = "ps5upload.payloadUpdatesSeen.v1";

/** What is new since `seen`. With nothing seen yet (first run), nothing is: the counters are
 *  only taken as the starting point, so an old update is never announced as news. */
export function payloadUpdateNotices(
  state: Pick<PayloadUpdateState, "rows" | "applied" | "available_seq" | "applied_seq">,
  seen: UpdateSeen | null,
): { notices: PayloadUpdateNotice[]; seen: UpdateSeen } {
  const now = { available_seq: state.available_seq, applied_seq: state.applied_seq };
  if (!seen) return { notices: [], seen: now };
  const notices: PayloadUpdateNotice[] = [];
  const newApplied = Math.max(0, state.applied_seq - seen.applied_seq);
  if (newApplied > 0) {
    const items = state.applied.slice(0, newApplied);
    if (items.length) notices.push({ kind: "applied", items });
  }
  if (state.available_seq > seen.available_seq) {
    const names = state.rows
      .filter((r) => r.update_available)
      .map((r) => r.display_name || r.payload_id);
    if (names.length) notices.push({ kind: "available", names });
  }
  return { notices, seen: now };
}

function loadSeen(): UpdateSeen | null {
  try {
    const v = JSON.parse(safeGetItem(KEY) ?? "null") as UpdateSeen | null;
    return v && typeof v.available_seq === "number" && typeof v.applied_seq === "number" ? v : null;
  } catch {
    return null;
  }
}

/** Mark what the Payloads screen has just shown as seen, so it is not announced again. */
export function markPayloadUpdatesSeen(state: Pick<PayloadUpdateState, "available_seq" | "applied_seq">): void {
  safeSetItem(KEY, JSON.stringify({ available_seq: state.available_seq, applied_seq: state.applied_seq }));
}

function announce(n: PayloadUpdateNotice): void {
  if (n.kind === "available") {
    pushNotification(
      "info",
      trStatic("pu_notice_available", "Payload updates available: {n}", { n: n.names.length }),
      { body: n.names.join(", "), link: "/payloads?tab=catalog" },
    );
    return;
  }
  const text = n.items
    .map((a) => `${a.display_name || a.payload_id} ${a.from ? `${a.from} → ` : ""}${a.to}`)
    .join(", ");
  pushNotification(
    "success",
    trStatic("pu_notice_applied", "Payloads updated: {n}", { n: n.items.length }),
    { body: text, link: "/payloads?tab=catalog" },
  );
}

/** Look at the engine's update state now and every 10 minutes; tell the user what is new. */
export function startPayloadUpdateNotices(): () => void {
  let stopped = false;
  const tick = async () => {
    try {
      const { state } = await payloadUpdatesOverview();
      if (stopped) return;
      const { notices, seen } = payloadUpdateNotices(state, loadSeen());
      safeSetItem(KEY, JSON.stringify(seen));
      notices.forEach(announce);
    } catch {
      /* the engine is down or older: try again later */
    }
  };
  const first = window.setTimeout(() => void tick(), 15_000);
  const every = window.setInterval(() => void tick(), 10 * 60_000);
  return () => {
    stopped = true;
    window.clearTimeout(first);
    window.clearInterval(every);
  };
}
