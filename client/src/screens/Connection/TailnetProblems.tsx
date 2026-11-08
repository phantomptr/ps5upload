import { useEffect, useState } from "react";

import { useTr } from "../../state/lang";
import { tailnetProblems, type TailnetState } from "../../api/tailnet";

/** How long a node has been connecting or waiting for its sign-in (0 otherwise), counted while
 *  it stays so; re-read every few seconds with the status. */
export function usePendingMs(state: TailnetState | undefined): number {
  const pending = state === "connecting" || state === "needs-login";
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (!pending) return;
    const start = Date.now();
    const t = window.setInterval(() => setElapsed(Date.now() - start), 5000);
    return () => {
      window.clearInterval(t);
      setElapsed(0);
    };
  }, [pending]);
  return pending ? elapsed : 0;
}

/**
 * A Tailscale node's trouble in plain words (Settings › Tailscale, the console's Tailscale panel):
 * a refused sign-in, signed out, the control server out of reach, the key expired or expiring, or
 * a sign-in that is taking too long. Nothing when all is well.
 */
export function TailnetProblems({
  st,
  testId,
}: {
  st: { state: TailnetState; error?: string; warning?: string; key_expiry?: number };
  testId: string;
}) {
  const tr = useTr();
  const pendingMs = usePendingMs(st.state);
  const problems = tailnetProblems(st, pendingMs);
  if (problems.length === 0) return null;
  return (
    <div className="flex flex-col gap-1" data-testid={testId}>
      {problems.map((p) => (
        <p key={p.key} className={`text-xs ${p.bad ? "text-[var(--color-bad)]" : "text-[var(--color-warn)]"}`}>
          {tr(p.key, p.vars, p.fallback)}
        </p>
      ))}
    </div>
  );
}
