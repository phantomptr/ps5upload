// "Found on your Tailscale network" (12.3): the PS5s this device's Tailscale can see that are not on
// its list yet, right on the Consoles page with Add. Before, they showed only in Settings ›
// Tailscale, or in Add a PS5 after a scan, so a phone away from home had nowhere obvious to start.
// It reads the engine's Tailscale look (cached there for a minute) while this device is on
// Tailscale, and nothing otherwise.

import { useEffect, useState } from "react";
import { Globe, Plus } from "lucide-react";

import { Button } from "../../../components";
import { useTr } from "../../../state/lang";
import { useRosterStore } from "../../../state/roster";
import { requestRosterPull } from "../../../state/rosterShare";
import { selfTailnet } from "../../../state/useTailnet";
import { addTailnetConsole, tailnetConsoles, type AppTailnetStatus, type FoundConsole } from "../../../api/tailnet";
import { consoleAddr } from "../../../lib/addr";
import { humanizeCaught } from "../../../lib/humanizeError";

/** How often the list is read again while the page is open (the engine answers from its cache). */
export const FOUND_READ_MS = 60_000;

/** The found consoles worth offering: online, not on this device's list (by the engine's word, or
 *  by either of their addresses), and not put aside this session. */
export function toOffer(
  found: readonly FoundConsole[],
  profiles: readonly { host: string }[],
  hidden: ReadonlySet<string>,
): FoundConsole[] {
  const listed = new Set(profiles.map((p) => consoleAddr(p.host).toLowerCase()));
  return found.filter(
    (c) =>
      c.online &&
      !c.listed &&
      !hidden.has(c.tailnet_ip) &&
      !listed.has(consoleAddr(c.tailnet_ip).toLowerCase()) &&
      !(c.lan_host && listed.has(consoleAddr(c.lan_host).toLowerCase())),
  );
}

const HIDDEN_KEY = "ps5upload.tsFound.hidden";

function readHidden(): Set<string> {
  try {
    return new Set(JSON.parse(sessionStorage.getItem(HIDDEN_KEY) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

export function TailnetFound({
  appTailnet,
  onAdded,
  onResume,
  resuming = false,
}: {
  appTailnet: AppTailnetStatus | null;
  onAdded: (id: string) => void;
  /** Turns this device's paused Tailscale back on. */
  onResume?: () => void;
  resuming?: boolean;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const on = selfTailnet(appTailnet) === "on";
  const [found, setFound] = useState<FoundConsole[]>([]);
  const [hidden, setHidden] = useState<Set<string>>(() => readHidden());
  const [adding, setAdding] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [err, setErr] = useState("");

  useEffect(() => {
    if (!on) {
      setFound([]);
      return;
    }
    let live = true;
    const read = () => {
      if (typeof document !== "undefined" && document.hidden) return;
      tailnetConsoles(false)
        .then((r) => live && setFound(r.consoles))
        .catch(() => {});
    };
    read();
    const id = window.setInterval(read, FOUND_READ_MS);
    return () => {
      live = false;
      window.clearInterval(id);
    };
  }, [on]);

  // The engine added it; the list mirrors in on the next pull: then open its card.
  useEffect(() => {
    if (pending && profiles.some((p) => p.id === pending)) {
      setPending(null);
      onAdded(pending);
    }
  }, [pending, profiles, onAdded]);

  const later = (c: FoundConsole) => {
    const next = new Set(hidden).add(c.tailnet_ip);
    setHidden(next);
    try {
      sessionStorage.setItem(HIDDEN_KEY, JSON.stringify([...next]));
    } catch {
      // Not remembered: it shows again on the next visit.
    }
  };
  const offer = toOffer(found, profiles, hidden);
  // Signed in but paused here: nothing can be found or reached through it until it's on again.
  if (selfTailnet(appTailnet) === "paused" && onResume && !hidden.has("paused")) {
    return (
      <section
        className="flex flex-wrap items-center gap-3 rounded-[var(--radius-panel)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-4"
        data-testid="consoles-ts-paused"
      >
        <Globe size={18} aria-hidden className="shrink-0 text-[var(--color-muted)]" />
        <p className="min-w-0 flex-1 text-sm">
          {tr(
            "cs_ts_paused",
            undefined,
            "Tailscale is paused on this device, so PS5s away from home can't be reached. Turn it on to use them from anywhere.",
          )}
        </p>
        <Button size="sm" variant="primary" loading={resuming} onClick={onResume} data-testid="consoles-ts-resume">
          {tr("cs_ts_resume", undefined, "Turn on")}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => later({ tailnet_ip: "paused" } as FoundConsole)}>
          {tr("cs_ask_later", undefined, "Later")}
        </Button>
      </section>
    );
  }
  if (!on || offer.length === 0) return null;

  const add = async (c: FoundConsole) => {
    setAdding(c.tailnet_ip);
    setErr("");
    try {
      const r = await addTailnetConsole(c.tailnet_ip, c.name);
      setPending(r.console.id);
      requestRosterPull();
    } catch (e) {
      setErr(
        tr(
          "cs_add_failed",
          { err: humanizeCaught(e) },
          "Couldn't add this PS5 ({err}). Check that it is on and that the address is right, then press Add again.",
        ),
      );
    } finally {
      setAdding(null);
    }
  };

  return (
    <section
      className="flex flex-col gap-3 rounded-[var(--radius-panel)] border border-[var(--color-accent)] bg-[var(--color-accent-soft)] p-4"
      data-testid="consoles-ts-found"
    >
      <div className="flex items-start gap-3">
        <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-[var(--color-surface)] text-[var(--color-accent-bright)]">
          <Globe size={18} aria-hidden />
        </span>
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">{tr("cs_ts_found_title", undefined, "Found on your Tailscale network")}</h2>
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "cs_ts_found_body",
              undefined,
              "These PS5s are on your Tailscale network but not on this device's list yet. Add one to use it from here, at home or away.",
            )}
          </p>
        </div>
      </div>
      <ul className="flex flex-col gap-2">
        {offer.map((c) => (
          <li
            key={c.tailnet_ip}
            className="flex flex-wrap items-center gap-2 rounded-[var(--radius-field)] bg-[var(--color-surface)] px-3 py-2"
            data-testid="consoles-ts-found-row"
          >
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium [overflow-wrap:anywhere]">{c.name || c.tailnet_name}</p>
              <p className="font-mono text-xs text-[var(--color-muted)]">
                {c.tailnet_ip}
                {c.lan_host ? ` · ${c.lan_host}` : ""}
              </p>
              {!c.reachable && (
                <p className="text-xs text-[var(--color-warn)]" data-testid="consoles-ts-found-blocked">
                  {tr(
                    "cs_ts_found_blocked",
                    undefined,
                    "It doesn't let this device in over Tailscale yet. Allow it on a device that already uses this PS5 (its card in Connection), or on the PS5's own page.",
                  )}
                </p>
              )}
            </div>
            <Button
              size="sm"
              variant="primary"
              leftIcon={<Plus size={14} />}
              loading={adding === c.tailnet_ip}
              disabled={!!adding}
              onClick={() => void add(c)}
              data-testid="consoles-ts-found-add"
            >
              {tr("tn_found_add", undefined, "Add")}
            </Button>
            <Button size="sm" variant="ghost" disabled={!!adding} onClick={() => later(c)} data-testid="consoles-ts-found-later">
              {tr("cs_ask_later", undefined, "Later")}
            </Button>
          </li>
        ))}
      </ul>
      {err && (
        <p role="alert" className="text-xs text-[var(--color-bad)]">
          {err}
        </p>
      )}
    </section>
  );
}
