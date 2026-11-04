import { useCallback, useEffect, useState } from "react";
import { ExternalLink, RefreshCw } from "lucide-react";

import { Badge, Button, Checkbox, ErrorCard, Spinner } from "../../components";
import {
  consolePorts,
  isOutgoing,
  portLink,
  type ConsolePort,
  type ConsolePorts,
  type PortScope,
} from "../../api/consolePorts";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import { humanizeCaught } from "../../lib/humanizeError";

/** The ports in use on the PS5, what usually listens on each, and a link to the ones a PC can
 *  open (Discord ask). The helper finds them by binding each port, never by connecting. */
export default function PortsPanel() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const [view, setView] = useState<ConsolePorts | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [showOutgoing, setShowOutgoing] = useState(false);

  const refresh = useCallback(async () => {
    const h = host.trim();
    if (!h) return;
    setLoading(true);
    setError(null);
    try {
      setView(await consolePorts(h));
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setLoading(false);
    }
  }, [host]);

  useEffect(() => {
    setView(null);
    void refresh();
  }, [refresh]);

  const scopeLabel = (s: PortScope): string => {
    if (s === "all") return tr("ports.reach_all", undefined, "Network");
    if (s === "console") return tr("ports.reach_console", undefined, "This PS5 only");
    if (s === "lan") return tr("ports.reach_lan", undefined, "Outgoing connection");
    return tr("ports.reach_busy", undefined, "In use");
  };

  if (error) {
    return (
      <ErrorCard
        title={tr("ports.error_title", undefined, "Couldn't list the PS5's ports")}
        detail={`${error}\n${tr(
          "ports.needs_helper",
          undefined,
          "This needs the ps5upload helper from 7.0.24 or later. Send the helper again from Connection.",
        )}`}
        onRetry={() => void refresh()}
      />
    );
  }
  if (!view) {
    return (
      <div className="flex items-center gap-2 text-sm text-[var(--color-muted)]">
        <Spinner size={14} />
        {tr("ports.scanning", undefined, "Checking every port on the PS5…")}
      </div>
    );
  }

  const outgoing = view.ports.filter(isOutgoing);
  const shown = showOutgoing ? view.ports : view.ports.filter((p) => !isOutgoing(p));

  const row = (p: ConsolePort) => {
    const link = portLink(host, p);
    return (
      <li
        key={`${p.proto}:${p.port}`}
        className="flex flex-wrap items-center gap-3 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3.5 py-2.5 text-sm"
      >
        <div className="flex min-w-[min(100%,14rem)] flex-1 flex-wrap items-center gap-2">
          <span className="font-mono font-medium">{p.port}</span>
          <Badge tone="neutral" variant="outline">
            {p.proto.toUpperCase()}
          </Badge>
          <span className={p.name ? "" : "text-[var(--color-muted)]"}>
            {p.name || tr("ports.unknown", undefined, "Unknown")}
          </span>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone={p.scope === "all" ? "good" : "neutral"} variant="outline">
            {scopeLabel(p.scope)}
          </Badge>
          {link && (
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<ExternalLink size={14} />}
              onClick={() => void openExternalUrl(link)}
              title={link}
            >
              {tr("ports.open", undefined, "Open")}
            </Button>
          )}
        </div>
      </li>
    );
  };

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm text-[var(--color-muted)]">
          {tr(
            "ports.summary",
            { n: shown.length, ms: view.elapsed_ms },
            `${shown.length} ports in use · checked in ${view.elapsed_ms} ms`,
          )}
        </span>
        <Button
          variant="secondary"
          size="sm"
          leftIcon={<RefreshCw size={14} />}
          disabled={loading}
          onClick={() => void refresh()}
        >
          {tr("ports.scan", undefined, "Scan again")}
        </Button>
      </div>
      {outgoing.length > 0 && (
        <Checkbox
          checked={showOutgoing}
          onChange={setShowOutgoing}
          label={tr(
            "ports.show_outgoing",
            { n: outgoing.length },
            `Show outgoing connections (${outgoing.length})`,
          )}
        />
      )}
      <ul className="flex flex-col gap-2">{shown.map(row)}</ul>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "ports.how",
          undefined,
          "Found by binding each port on the PS5 and letting go at once, so no payload sees a connection. Names are guesses from the port number. Open tries http:// in your browser; not every service has a web page.",
        )}
      </p>
    </div>
  );
}
