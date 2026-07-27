import { useCallback, useEffect, useState } from "react";
import {
  MonitorPlay,
  Loader2,
  RefreshCw,
  X,
  KeyRound,
  Clock,
  User,
  Copy,
  Check,
} from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { useDocumentVisible } from "../../lib/visibility";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { transferAddr } from "../../lib/addr";
import {
  remoteplayRequest,
  remoteplayStatus,
  remoteplayCancel,
  type RemotePlayStatus,
} from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";

export default function RemotePlayScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const visible = useDocumentVisible();
  const guard = useStaleHostGuard();

  const [manualAccountId, setManualAccountId] = useState("");
  const [status, setStatus] = useState<RemotePlayStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const isActive =
    status?.state === "starting" || status?.state === "waiting";

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    const probe = guard.capture();
    try {
      const s = await remoteplayStatus(addr);
      if (probe.isStale()) return;
      setStatus(s);
      setError(null);
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    }
  }, [addr, payloadStatus, guard]);

  useEffect(() => {
    if (!addr || payloadStatus !== "up") return;
    void refresh();
  }, [addr, payloadStatus, refresh]);

  // Auto-refresh every 2s while starting/waiting, but only when the
  // tab is visible — avoids wasteful polling in the background.
  useEffect(() => {
    if (!isActive || !visible) return;
    const id = window.setInterval(() => {
      void refresh();
    }, 2_000);
    return () => window.clearInterval(id);
  }, [refresh, isActive, visible]);

  const handleCopyPin = useCallback(async () => {
    if (!status?.pin) return;
    try {
      await navigator.clipboard.writeText(status.pin);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      // clipboard may be unavailable; silently ignore
    }
  }, [status]);

  const handleRequest = useCallback(async () => {
    if (!addr) return;
    const probe = guard.capture();
    setBusy(true);
    setError(null);
    try {
      await remoteplayRequest(manualAccountId.trim() || undefined, addr);
      if (probe.isStale()) return;
      await refresh();
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      setBusy(false);
    }
  }, [addr, manualAccountId, refresh, guard]);

  const handleCancel = useCallback(async () => {
    if (!addr) return;
    const probe = guard.capture();
    setBusy(true);
    setError(null);
    try {
      await remoteplayCancel(addr);
      if (probe.isStale()) return;
      await refresh();
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      setBusy(false);
    }
  }, [addr, refresh, guard]);

  return (
    <div className="mx-auto max-w-4xl space-y-4 p-4">
      <PageHeader
        icon={MonitorPlay}
        title={tr("remotePlay_title", undefined, "Remote Play")}
        description={tr(
          "remotePlay_subtitle",
          undefined,
          "Generate a Remote Play PIN to connect from the PS Remote Play app",
        )}
        right={
          <Button
            variant="ghost"
            size="sm"
            onClick={refresh}
            disabled={busy || payloadStatus !== "up" || !addr}
          >
            <RefreshCw size={14} />
          </Button>
        }
      />

      <ConnectionGate>
        {error && <ErrorCard title={error} />}

        {/* Request form */}
        <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
          <div className="space-y-3">
            <div>
              <label className="mb-1 block text-xs text-[var(--color-muted)]">
                {tr(
                  "remotePlay_account_id_label",
                  undefined,
                  "Manual account ID (optional)",
                )}
              </label>
              <input
                type="text"
                value={manualAccountId}
                onChange={(e) => setManualAccountId(e.target.value)}
                placeholder={tr(
                  "remotePlay_account_id_placeholder",
                  undefined,
                  "auto-detect",
                )}
                className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-sm text-[var(--color-text)] placeholder:text-[var(--color-muted)] focus:border-[var(--color-accent)] focus:outline-none"
              />
            </div>
            <div className="flex gap-2">
              <Button
                variant="primary"
                onClick={handleRequest}
                disabled={busy || payloadStatus !== "up" || !addr}
              >
                {busy ? (
                  <Loader2 size={14} className="animate-spin" />
                ) : (
                  <MonitorPlay size={14} />
                )}
                {tr("remotePlay_request", undefined, "Request PIN")}
              </Button>
              {isActive && (
                <Button
                  variant="danger"
                  onClick={handleCancel}
                  disabled={busy}
                >
                  <X size={14} />
                  {tr("remotePlay_cancel", undefined, "Cancel")}
                </Button>
              )}
            </div>
          </div>
        </div>

        {/* Status display */}
        {status && (
          <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
            <h3 className="mb-3 text-sm font-medium text-[var(--color-text)]">
              {tr("remotePlay_status", undefined, "Status")}
            </h3>
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
              <dt className="text-[var(--color-muted)]">
                {tr("remotePlay_field_state", undefined, "state")}
              </dt>
              <dd className="flex items-center gap-2 font-mono">
                <span
                  className={`inline-block h-2 w-2 rounded-full ${
                    isActive
                      ? "bg-[var(--color-good)]"
                      : "bg-[var(--color-muted)]"
                  }`}
                />
                {status.state}
              </dd>
              <dt className="flex items-center gap-1 text-[var(--color-muted)]">
                <KeyRound size={12} />
                {tr("remotePlay_field_pin", undefined, "PIN")}
              </dt>
              <dd className="flex items-center gap-2">
                <span className="font-mono text-lg tracking-widest text-[var(--color-accent)]">
                  {status.pin || "—"}
                </span>
                {status.pin && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={handleCopyPin}
                    className="text-[var(--color-muted)]"
                  >
                    {copied ? (
                      <Check size={12} className="text-[var(--color-good)]" />
                    ) : (
                      <Copy size={12} />
                    )}
                  </Button>
                )}
              </dd>
              <dt className="flex items-center gap-1 text-[var(--color-muted)]">
                <User size={12} />
                {tr(
                  "remotePlay_field_account_id",
                  undefined,
                  "account_id",
                )}
              </dt>
              <dd className="font-mono break-all">
                {status.account_id || "—"}
              </dd>
              <dt className="flex items-center gap-1 text-[var(--color-muted)]">
                <Clock size={12} />
                {tr(
                  "remotePlay_field_seconds_left",
                  undefined,
                  "seconds_left",
                )}
              </dt>
              <dd className="font-mono tabular-nums">
                {status.seconds_left > 0 ? status.seconds_left : "—"}
              </dd>
            </dl>
            {status.err && (
              <div className="mt-3 rounded-md border border-[var(--color-error)] bg-[var(--color-error-bg,transparent)] p-3">
                <p className="text-xs text-[var(--color-error)]">
                  {status.err}
                </p>
              </div>
            )}
          </div>
        )}
      </ConnectionGate>
    </div>
  );
}
