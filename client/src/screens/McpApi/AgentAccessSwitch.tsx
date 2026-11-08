import { useCallback, useEffect, useState } from "react";

import { Toggle } from "../../components";
import { useTr, type Translator } from "../../state/lang";
import { readAgentAccess, writeAgentAccess, type AgentAccess } from "./model";

type Tr = Translator;

export interface AgentAccessViewProps {
  tr: Tr;
  /** null: not read yet, or the engine did not answer. */
  access: AgentAccess | null;
  busy: boolean;
  error: string;
  onChange: (canChange: boolean) => void;
}

/** "Agents can change things": on is full access (the default), off lets agents only look. The
 *  engine's environment can hold it off; then it shows off, and cannot be switched here. */
export function AgentAccessView({ tr, access, busy, error, onChange }: AgentAccessViewProps) {
  const locked = access?.locked === true;
  return (
    <div className="space-y-2" data-testid="mcp-agent-access">
      <Toggle
        checked={access !== null && !access.read_only}
        disabled={access === null || locked || busy}
        onChange={onChange}
        label={tr("mcp_access_label", undefined, "Agents can change things")}
        hint={tr(
          "mcp_access_hint",
          undefined,
          "Off: agents can only look, such as listing files, reading status and following transfers. Nothing changes.",
        )}
      />
      {locked && (
        <p className="text-xs text-[var(--color-muted)]">
          {tr("mcp_access_locked", undefined, "Off, set by the engine's environment.")}
        </p>
      )}
      {error && (
        <p className="text-xs text-[var(--color-bad)]" role="alert">
          {tr("mcp_access_failed", { error }, `Couldn't change it: ${error}`)}
        </p>
      )}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "mcp_access_limit",
          undefined,
          "This stops agents using PS5Upload's tools; a program that can run commands on this computer could still reach the engine.",
        )}
      </p>
    </div>
  );
}

/** The switch, read from and saved to the engine (`/api/mcp/access`). */
export function AgentAccessSwitch({ engineUrl }: { engineUrl: string }) {
  const tr = useTr();
  const [access, setAccess] = useState<AgentAccess | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const stop = new AbortController();
    void readAgentAccess(engineUrl, stop.signal)
      .then(setAccess)
      .catch(() => setAccess(null));
    return () => stop.abort();
  }, [engineUrl]);

  const onChange = useCallback(
    (canChange: boolean) => {
      setBusy(true);
      setError("");
      void writeAgentAccess(engineUrl, !canChange)
        .then(setAccess)
        .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
        .finally(() => setBusy(false));
    },
    [engineUrl],
  );

  return (
    <AgentAccessView tr={tr} access={access} busy={busy} error={error} onChange={onChange} />
  );
}
