import { useCallback, useEffect, useMemo, useState } from "react";
import {
  MonitorPlay,
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
  Spinner,
} from "../../components";
import { writeClipboard } from "../../lib/clipboard";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { useDocumentVisible } from "../../lib/visibility";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { transferAddr } from "../../lib/addr";
import { accountIdToChiakiNumeric } from "../../lib/remoteplay";
import {
  remoteplayRequest,
  remoteplayStatus,
  remoteplayCancel,
  remoteplayReadiness,
  remoteplayEnable,
  type RemotePlayStatus,
  type RemotePlayReadiness,
} from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";

/** Firmware magic to a human version, e.g. 0x09600004 -> "9.60".
 *
 *  Both bytes are BCD, not binary: 9.60 is 0x0960 and 10.00 is 0x1000.
 *  Reading them as plain integers gives "9.96" and "16.0" — which is
 *  exactly what this screen displayed before. The magic also carries
 *  low-order bits past the version, so it is never shown raw. */
export function formatFirmware(magic: number): string {
  const bcd = (b: number) => (b >> 4) * 10 + (b & 0x0f);
  const major = bcd((magic >> 24) & 0xff);
  const minor = bcd((magic >> 16) & 0xff);
  return `${major}.${minor < 10 ? "0" : ""}${minor}`;
}

/** Readiness checklist: every precondition Remote Play needs, and where a
 *  row can be fixed, the button that fixes it.
 *
 *  The point is that nobody should have to know the recipe, read a guide,
 *  or decode a Sony error code. A failing row says what is wrong in plain
 *  words and either fixes it or says what to do instead. */
function ReadinessPanel({
  readiness,
  onEnable,
  busy,
  tr,
}: {
  readiness: RemotePlayReadiness | null;
  onEnable: (scope: "service" | "user") => void;
  busy: boolean;
  tr: ReturnType<typeof useTr>;
}) {
  if (!readiness) return null;

  // Non-zero registry_err means we could not READ the console's settings.
  // Everything else in the snapshot is meaningless then, so say that
  // rather than reporting a pile of plausible-looking "off" rows.
  if (readiness.registry_err !== 0) {
    return (
      <ErrorCard
        title={tr(
          "remotePlay_registry_unreadable",
          undefined,
          "Couldn't read this console's settings, so Remote Play status is unknown. Reload the helper and try again.",
        )}
      />
    );
  }

  const fw = readiness.fw_magic ? formatFirmware(readiness.fw_magic) : null;

  const activated =
    readiness.account_id_raw !== 0 && readiness.account_type === "np";

  const rows: {
    key: string;
    ok: boolean;
    label: string;
    detail?: string;
    fix?: () => void;
    fixLabel?: string;
    hint?: string;
  }[] = [
    {
      key: "user",
      ok: readiness.foreground_uid !== 0,
      label: tr("remotePlay_check_user", undefined, "Someone is signed in"),
      detail:
        readiness.user_slot > 0
          ? tr("remotePlay_slot", { slot: readiness.user_slot }, `user ${readiness.user_slot}`)
          : undefined,
      hint: tr(
        "remotePlay_check_user_hint",
        undefined,
        "Sign in on the console — Remote Play pairs with whoever is on screen.",
      ),
    },
    {
      key: "account",
      ok: activated,
      label: tr("remotePlay_check_account", undefined, "Account is activated"),
      hint: tr(
        "remotePlay_check_account_hint",
        undefined,
        "This account has never been activated, so it cannot pair. Activating changes account data and can affect save games, so ps5upload does not do it for you yet.",
      ),
    },
    {
      key: "service",
      ok: readiness.service_enabled !== 0,
      label: tr("remotePlay_check_service", undefined, "Remote Play is on"),
      fix: () => onEnable("service"),
      fixLabel: tr("remotePlay_turn_on", undefined, "Turn on"),
    },
  ];

  // Only shown on firmware that actually has the setting.
  if (readiness.has_per_user !== 0) {
    rows.push({
      key: "peruser",
      ok: readiness.user_enabled !== 0,
      label: tr(
        "remotePlay_check_peruser",
        undefined,
        "Remote Play is allowed for this user",
      ),
      detail: tr("remotePlay_fw10", undefined, "firmware 10.00 and later"),
      fix: () => onEnable("user"),
      fixLabel: tr("remotePlay_allow_user", undefined, "Allow"),
    });
  }

  return (
    <div className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4">
      <div className="mb-3 flex items-center justify-between">
        <span className="text-sm font-medium">
          {tr("remotePlay_readiness", undefined, "Before you pair")}
        </span>
        {fw && (
          <span className="text-xs text-[var(--color-muted)]">
            {tr("remotePlay_fw", { fw }, `firmware ${fw}`)}
          </span>
        )}
      </div>
      <ul className="space-y-2">
        {rows.map((r) => (
          <li key={r.key} className="flex items-start gap-2 text-sm">
            <span className={r.ok ? "text-emerald-500" : "text-amber-500"}>
              {r.ok ? <Check size={16} /> : <X size={16} />}
            </span>
            <span className="flex-1">
              <span className={r.ok ? "" : "font-medium"}>{r.label}</span>
              {r.detail && (
                <span className="ml-2 text-xs text-[var(--color-muted)]">
                  {r.detail}
                </span>
              )}
              {!r.ok && r.hint && (
                <div className="mt-0.5 text-xs text-[var(--color-muted)]">
                  {r.hint}
                </div>
              )}
            </span>
            {!r.ok && r.fix && (
              <Button size="sm" variant="secondary" onClick={r.fix} disabled={busy}>
                {r.fixLabel}
              </Button>
            )}
          </li>
        ))}
      </ul>
    </div>
  );
}

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
  const [copiedAcct, setCopiedAcct] = useState(false);
  const [copiedChiaki, setCopiedChiaki] = useState(false);
  const [readiness, setReadiness] = useState<RemotePlayReadiness | null>(null);

  /* Refreshed alongside status, and again after any enable, so the panel
   * always reflects what the console actually holds rather than what we
   * asked it to do. */
  const refreshReadiness = useCallback(async () => {
    if (!addr) return;
    try {
      setReadiness(await remoteplayReadiness(addr));
    } catch {
      /* Non-fatal: the checklist simply does not render. The pairing flow
       * below still works and reports its own errors. */
      setReadiness(null);
    }
  }, [addr]);

  useEffect(() => {
    void refreshReadiness();
  }, [refreshReadiness]);

  const handleEnable = useCallback(
    async (scope: "service" | "user") => {
      if (!addr) return;
      setBusy(true);
      setError(null);
      try {
        setReadiness(await remoteplayEnable(scope, addr));
      } catch (e) {
        setError(humanizePs5Error(e instanceof Error ? e.message : String(e)));
      } finally {
        setBusy(false);
      }
    },
    [addr],
  );

  const accountId = status?.account_id;
  const chiakiNumeric = useMemo(
    () => (accountId ? accountIdToChiakiNumeric(accountId) : ""),
    [accountId],
  );

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
    // writeClipboard, not navigator.clipboard: the async Clipboard API is
    // secure-context-only, so it is absent both in the packaged webview and
    // on the self-hosted UI served over plain HTTP from a LAN address. The
    // helper falls back to execCommand so the button actually copies there.
    if (await writeClipboard(status.pin)) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
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

        <ReadinessPanel
          readiness={readiness}
          onEnable={handleEnable}
          busy={busy}
          tr={tr}
        />

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
                size="md"
                onClick={handleRequest}
                disabled={busy || payloadStatus !== "up" || !addr}
              >
                {busy ? (
                  <Spinner size={14} tone="inherit" />
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
              <dd className="space-y-1">
                {/* Base64 account ID (Chiaki / chiaki-ng) */}
                <div className="flex items-center gap-2">
                  <span className="font-mono break-all text-sm">
                    {status.account_id || "—"}
                  </span>
                  {status.account_id && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        if (await writeClipboard(status.account_id)) {
                          setCopiedAcct(true);
                          setTimeout(() => setCopiedAcct(false), 2_000);
                        }
                      }}
                      className="shrink-0 text-[var(--color-muted)]"
                    >
                      {copiedAcct ? (
                        <Check size={12} className="text-[var(--color-good)]" />
                      ) : (
                        <Copy size={12} />
                      )}
                    </Button>
                  )}
                </div>
                {/* Numeric ID for pxplay / Chiaki classic */}
                {chiakiNumeric && (
                  <div className="flex items-center gap-2">
                    <span className="text-xs text-[var(--color-muted)]">{tr("remotePlay_pxplay_label", "pxplay:")}</span>
                    <span className="font-mono break-all text-sm text-[var(--color-text)]">
                      {chiakiNumeric}
                    </span>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        if (await writeClipboard(chiakiNumeric)) {
                          setCopiedChiaki(true);
                          setTimeout(() => setCopiedChiaki(false), 2_000);
                        }
                      }}
                      className="shrink-0 text-[var(--color-muted)]"
                    >
                      {copiedChiaki ? (
                        <Check size={12} className="text-[var(--color-good)]" />
                      ) : (
                        <Copy size={12} />
                      )}
                    </Button>
                  </div>
                )}
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
