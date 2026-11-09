// "Pixel wants to use Living-room PS5: Allow / Decline", over every screen of an app the console
// already trusts. The asks come from each console's mesh picture (state/mesh, polled for the
// selected console everywhere and for every console the Consoles page shows); answering goes to the
// console, so the console's own page and every other trusted app see the answer too.

import { useState } from "react";
import { useLocation } from "react-router";
import { ShieldCheck, X } from "lucide-react";

import { Button } from "../../../components";
import { meshDevice, type MeshAsk } from "../../../api/mesh";
import { pollMesh, resetMeshCursor, useMeshStore } from "../../../state/mesh";
import { useRosterStore } from "../../../state/roster";
import { useConnectionStore } from "../../../state/connection";
import { hostOf } from "../../../lib/addr";
import { humanizeCaught } from "../../../lib/humanizeError";
import { useTr } from "../../../state/lang";

export interface PendingAsk {
  host: string;
  consoleName: string;
  ask: MeshAsk;
}

/** Every ask on every console this app knows, oldest first, less the ones put off with Later.
 *  Only from a picture that is current: not when the last read failed, nor from a console whose
 *  helper is not answering (`isUp`), whose old asks may have been answered or run out since. */
export function pendingAsks(
  byHost: Record<string, { state: { asks?: MeshAsk[] } | null; error?: string | null }>,
  profiles: readonly { host: string; name: string }[],
  later: ReadonlySet<string>,
  isUp: (host: string) => boolean = () => true,
): PendingAsk[] {
  const out: PendingAsk[] = [];
  for (const [host, m] of Object.entries(byHost)) {
    const p = profiles.find((x) => hostOf(x.host) === hostOf(host));
    if (!p || m.error || !isUp(host)) continue;
    for (const a of m.state?.asks ?? []) {
      if (later.has(`${hostOf(host)}:${a.id}`)) continue;
      out.push({ host, consoleName: p.name, ask: a });
    }
  }
  return out.sort((a, b) => a.ask.at - b.ask.at);
}

/** The prompt itself (render-tested on its own). */
export function AskPrompt({
  pending,
  more,
  busy,
  error,
  onAnswer,
  onLater,
}: {
  pending: PendingAsk;
  more: number;
  busy: boolean;
  error: string | null;
  onAnswer: (op: "allow" | "decline") => void;
  onLater: () => void;
}) {
  const tr = useTr();
  const { ask, consoleName } = pending;
  return (
    <div
      role="alertdialog"
      aria-labelledby="console-ask-title"
      className="glass-float elev-3 pointer-events-auto w-[min(26rem,calc(100vw-2rem))] rounded-[var(--radius-panel)] p-4"
      data-testid="console-ask-prompt"
    >
      <div className="flex items-start gap-3">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
          <ShieldCheck size={18} aria-hidden />
        </span>
        <div className="min-w-0 flex-1">
          <p id="console-ask-title" className="text-sm font-semibold">
            {tr(
              "cs_ask_line",
              { device: ask.name || ask.ip || "A device", console: consoleName },
              "{device} wants to use {console}",
            )}
          </p>
          <p className="mt-0.5 text-xs text-[var(--color-muted)]">
            {tr(
              "cs_ask_body",
              { ip: ask.ip || "?" },
              "It asked from {ip}. Allow it only if you know the device: it can then upload, install and manage files on this PS5.",
            )}
          </p>
          {more > 0 ? (
            <p className="mt-1 text-xs text-[var(--color-muted)]">
              {tr("cs_ask_more", { count: more }, "{count} more waiting")}
            </p>
          ) : null}
          {error ? (
            <p role="alert" className="mt-1 text-xs text-[var(--color-bad)]">
              {error}
            </p>
          ) : null}
        </div>
        <button
          type="button"
          onClick={onLater}
          aria-label={tr("cs_ask_later", undefined, "Later")}
          title={tr("cs_ask_later", undefined, "Later")}
          className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        >
          <X size={14} />
        </button>
      </div>
      <div className="mt-3 flex justify-end gap-2">
        <Button size="sm" variant="ghost" disabled={busy} onClick={() => onAnswer("decline")} data-testid="console-ask-decline">
          {tr("cs_ask_decline", undefined, "Decline")}
        </Button>
        <Button size="sm" variant="primary" loading={busy} onClick={() => onAnswer("allow")} data-testid="console-ask-allow">
          {tr("cs_ask_allow", undefined, "Allow")}
        </Button>
      </div>
    </div>
  );
}

/** Whether a path is the Consoles page, where each card answers its own asks (the prompt would
 *  show the same ask twice there). */
export function onConsolesPage(pathname: string, search: string): boolean {
  if (pathname !== "/connection") return false;
  const tab = new URLSearchParams(search).get("tab");
  return tab !== "devices" && tab !== "health";
}

/** Mounted once, over every screen but the Consoles page. */
export function ConsoleAsks() {
  const byHost = useMeshStore((s) => s.byHost);
  const profiles = useRosterStore((s) => s.profiles);
  const runtimeByHost = useConnectionStore((s) => s.runtimeByHost);
  const loc = useLocation();
  const [later, setLater] = useState<Set<string>>(() => new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const all = pendingAsks(byHost, profiles, later, (h) => runtimeByHost[hostOf(h)]?.payloadStatus === "up");
  const first = all[0];
  if (!first || onConsolesPage(loc.pathname, loc.search)) return null;
  const answer = async (op: "allow" | "decline") => {
    setBusy(true);
    setError(null);
    try {
      await meshDevice(first.host, op, first.ask.id);
      resetMeshCursor(first.host);
      await pollMesh(first.host);
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="pointer-events-none fixed inset-x-0 bottom-[calc(env(safe-area-inset-bottom)+5rem)] z-50 flex justify-center px-4 md:bottom-6 md:justify-end md:px-6">
      <AskPrompt
        pending={first}
        more={all.length - 1}
        busy={busy}
        error={error}
        onAnswer={(op) => void answer(op)}
        onLater={() => setLater((s) => new Set(s).add(`${hostOf(first.host)}:${first.ask.id}`))}
      />
    </div>
  );
}
