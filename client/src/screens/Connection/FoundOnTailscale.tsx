import { useCallback, useEffect, useState } from "react";
import { Globe, Loader2, Plus, RefreshCw } from "lucide-react";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { humanizeCaught } from "../../lib/humanizeError";
import {
  addTailnetConsole,
  rosterMatch,
  tailnetConsoles,
  type FoundConsole,
  type FoundConsoles,
} from "../../api/tailnet";

/** The console list is mirrored from the engine (state/rosterShare.ts) on focus: ask for that now
 *  instead of waiting for its next pull, so an added console shows at once. */
function pullConsoleList() {
  window.dispatchEvent(new Event("focus"));
}

/**
 * "Found on your Tailscale network": the PS5Upload consoles this device can reach over Tailscale,
 * met at home or not, each with Add (or Connect when it is on the list already), and "Enter a
 * Tailscale address" for a phone that has only the Tailscale app (no list to read). Adding goes
 * through the engine, which keeps one entry per console: at its home address when known, so the
 * home network is still used at home. Self-contained: place it wherever consoles are added.
 * Asks the engine once when shown and on "Look again"; nothing runs in the background.
 */
export default function FoundOnTailscale({ className = "" }: { className?: string }) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const setActive = useRosterStore((s) => s.setActive);
  const [found, setFound] = useState<FoundConsoles | null>(null);
  const [looking, setLooking] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState("");
  const [typed, setTyped] = useState("");
  /** An added console, selected once the list mirror brings it in. */
  const [pendingId, setPendingId] = useState<string | null>(null);

  const look = useCallback(async (refresh: boolean) => {
    setLooking(true);
    setErr("");
    try {
      setFound(await tailnetConsoles(refresh));
    } catch (e) {
      setErr(humanizeCaught(e));
    } finally {
      setLooking(false);
    }
  }, []);

  useEffect(() => {
    void look(false);
  }, [look]);

  useEffect(() => {
    if (pendingId && profiles.some((p) => p.id === pendingId)) {
      setActive(pendingId);
      setPendingId(null);
    }
  }, [pendingId, profiles, setActive]);

  const add = async (addr: string, name = "") => {
    setBusy(addr);
    setErr("");
    try {
      const r = await addTailnetConsole(addr, name);
      setFound((f) =>
        f
          ? {
              ...f,
              consoles: [
                ...f.consoles.filter((c) => c.tailnet_ip !== r.found.tailnet_ip),
                { ...r.found, listed: r.console },
              ],
            }
          : f,
      );
      setPendingId(r.console.id);
      setTyped("");
      pullConsoleList();
    } catch (e) {
      setErr(tr("tn_found_add_failed", { err: humanizeCaught(e) }, "Could not add this PS5: {err}"));
    } finally {
      setBusy(null);
    }
  };

  return (
    <FoundView
      className={className}
      found={found}
      profiles={profiles}
      looking={looking}
      busy={busy}
      typed={typed}
      err={err}
      onLook={() => void look(true)}
      onAdd={(addr, name) => void add(addr, name)}
      onConnect={(id) => setActive(id)}
      onTyped={setTyped}
    />
  );
}

export interface FoundViewProps {
  className?: string;
  found: FoundConsoles | null;
  profiles: readonly { id: string; name: string; host: string }[];
  looking: boolean;
  /** The address being added. */
  busy: string | null;
  typed: string;
  err: string;
  onLook: () => void;
  onAdd: (addr: string, name?: string) => void;
  onConnect: (id: string) => void;
  onTyped: (v: string) => void;
}

/** What {@link FoundOnTailscale} shows, from its state: render-tested on its own. */
export function FoundView({
  className = "",
  found,
  profiles,
  looking,
  busy,
  typed,
  err,
  onLook,
  onAdd,
  onConnect,
  onTyped,
}: FoundViewProps) {
  const tr = useTr();
  const consoles = found?.consoles ?? [];
  const noTailscale = found?.backend === "none";
  return (
    <section
      className={`flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3 ${className}`}
      data-testid="tailnet-found"
    >
      <div className="flex items-center gap-2 text-sm font-semibold">
        <Globe size={16} />
        <span className="flex-1">{tr("tn_found_title", undefined, "Found on your Tailscale network")}</span>
        <Button
          size="sm"
          variant="ghost"
          leftIcon={looking ? <Loader2 size={12} className="animate-spin" /> : <RefreshCw size={12} />}
          disabled={looking}
          onClick={onLook}
          data-testid="tailnet-found-refresh"
        >
          {tr("tn_found_look_again", undefined, "Look again")}
        </Button>
      </div>
      {noTailscale ? (
        <p className="text-xs text-[var(--color-muted)]" data-testid="tailnet-found-off">
          {tr(
            "tn_found_off",
            undefined,
            "This device isn't on Tailscale. Sign in to Tailscale in Settings › Tailscale to find your PS5s from anywhere.",
          )}
        </p>
      ) : found && !found.can_list ? (
        <p className="text-xs text-[var(--color-muted)]" data-testid="tailnet-found-no-list">
          {tr(
            "tn_found_no_list",
            undefined,
            "PS5Upload can't see your Tailscale devices through the Tailscale app. Type your PS5's Tailscale address below, or sign PS5Upload in to Tailscale (Settings › Tailscale) to find your PS5s by itself.",
          )}
        </p>
      ) : found && consoles.length === 0 ? (
        <p className="text-xs text-[var(--color-muted)]" data-testid="tailnet-found-none">
          {tr(
            "tn_found_none",
            undefined,
            "No PS5 found on your Tailscale network. Put a PS5 on Tailscale first (Connection › Tailscale for this PS5, at home).",
          )}
        </p>
      ) : null}
      {consoles.length > 0 && (
        <ul className="flex flex-col divide-y divide-[var(--color-border)]" data-testid="tailnet-found-list">
          {consoles.map((c) => (
            <FoundRow
              key={c.tailnet_ip}
              c={c}
              busy={busy === c.tailnet_ip}
              inList={rosterMatch(c, profiles)}
              onAdd={() => onAdd(c.tailnet_ip, c.name)}
              onConnect={onConnect}
            />
          ))}
        </ul>
      )}
      {!noTailscale && (
        <form
          className="flex flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (typed.trim()) onAdd(typed.trim());
          }}
        >
          <label className="flex min-w-0 flex-1 flex-col gap-1 text-xs">
            {tr("tn_found_enter", undefined, "Enter a Tailscale address")}
            <input
              className="input max-md:min-h-11"
              value={typed}
              onChange={(e) => onTyped(e.target.value)}
              placeholder={tr("tn_found_enter_placeholder", undefined, "100.x.y.z or the PS5's name in Tailscale")}
              inputMode="url"
              autoCapitalize="off"
              autoCorrect="off"
              spellCheck={false}
              data-testid="tailnet-found-input"
            />
          </label>
          <Button
            type="submit"
            size="sm"
            variant="secondary"
            leftIcon={<Plus size={12} />}
            disabled={!typed.trim() || busy !== null}
            loading={busy === typed.trim()}
            data-testid="tailnet-found-add-typed"
          >
            {tr("tn_found_add", undefined, "Add")}
          </Button>
        </form>
      )}
      {err ? (
        <p className="text-xs text-[var(--color-bad)]" role="alert">
          {err}
        </p>
      ) : null}
    </section>
  );
}

function FoundRow({
  c,
  busy,
  inList,
  onAdd,
  onConnect,
}: {
  c: FoundConsole;
  busy: boolean;
  inList: { id: string; name: string } | null;
  onAdd: () => void;
  onConnect: (id: string) => void;
}) {
  const tr = useTr();
  return (
    <li className="flex items-center gap-3 py-2 text-xs" data-testid="tailnet-found-row">
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="truncate text-sm font-semibold">{c.name || c.tailnet_ip}</span>
          <span className="font-mono text-[var(--color-muted)]">{c.tailnet_ip}</span>
        </div>
        <div className="text-[var(--color-muted)]">
          {c.reachable
            ? c.lan_host
              ? tr("tn_found_answers_home", { host: c.lan_host }, "Answers over Tailscale · at home {host}")
              : tr("tn_found_answers", undefined, "Answers over Tailscale")
            : tr(
                "tn_found_no_answer",
                undefined,
                "Doesn't answer yet: its helper may be off, or it lets only listed devices in (it then shows this device on its Tailscale panel, with Allow).",
              )}
        </div>
      </div>
      {inList ? (
        <Button size="sm" variant="secondary" onClick={() => onConnect(inList.id)} data-testid="tailnet-found-connect">
          {tr("tn_found_connect", undefined, "Connect")}
        </Button>
      ) : (
        <Button
          size="sm"
          variant="primary"
          leftIcon={<Plus size={12} />}
          loading={busy}
          disabled={busy}
          onClick={onAdd}
          data-testid="tailnet-found-add"
        >
          {tr("tn_found_add", undefined, "Add")}
        </Button>
      )}
    </li>
  );
}
