import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useLocation, useNavigate } from "react-router";
import {
  MonitorPlay,
  Gamepad2,
  RefreshCw,
  X,
  KeyRound,
  Clock,
  User,
  Copy,
  Check,
} from "lucide-react";
import { Button, Callout, ErrorCard, Spinner } from "../../components";
import { writeClipboard } from "../../lib/clipboard";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { useDocumentVisible } from "../../lib/visibility";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { transferAddr } from "../../lib/addr";
import { accountIdToChiakiNumeric, accountIdToHex, accountIdToPairingB64 } from "../../lib/remoteplay";
import {
  remoteplayRequest,
  remoteplayStatus,
  remoteplayCancel,
  remoteplayReadiness,
  type RemotePlayStatus,
  type RemotePlayReadiness,
} from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";

/** The anchor old /remote-play links land on: /accounts#remote-play scrolls here. */
export const REMOTE_PLAY_ANCHOR = "remote-play";
const SIGN_IN_ANCHOR = "remote-play-signin";

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

/**
 * Remote Play, as a section of Accounts: what the console still needs (the checklist, with a
 * fix on each row), signing the account in (`signIn`, Accounts' np-fake-signin step, which is
 * what unlocks the console's own Remote Play switch), then the pairing PIN.
 *
 * It was its own screen ("Remote Play PIN") that sent people to Profile and back for the
 * sign-in; both now sit on one page, in the order they are done.
 */
export function RemotePlaySection({
  signIn,
}: {
  signIn?: ReactNode;
}) {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const visible = useDocumentVisible();
  const guard = useStaleHostGuard();
  const navigate = useNavigate();

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

  // An old /remote-play link lands on /accounts#remote-play: bring the section into view once.
  const { hash } = useLocation();
  const sectionRef = useRef<HTMLElement>(null);
  const scrolled = useRef(false);
  useEffect(() => {
    if (hash !== `#${REMOTE_PLAY_ANCHOR}` || scrolled.current) return;
    scrolled.current = true;
    sectionRef.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  }, [hash]);

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
    // The id may be typed as hex, base64 or pxplay's number; the payload
    // shows it back as given, so send the console's base64 spelling.
    const typed = manualAccountId.trim();
    const manual = typed ? accountIdToPairingB64(typed) : null;
    if (typed && !manual) {
      setError(
        tr(
          "remotePlay_account_id_invalid",
          undefined,
          "That is not an account ID. Use the hex ID (with or without 0x), the base64 ID, or the pxplay number.",
        ),
      );
      return;
    }
    const probe = guard.capture();
    setBusy(true);
    setError(null);
    try {
      await remoteplayRequest(manual ?? undefined, addr);
      if (probe.isStale()) return;
      await refresh();
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      setBusy(false);
    }
  }, [addr, manualAccountId, refresh, guard, tr]);

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
    <section
      ref={sectionRef}
      id={REMOTE_PLAY_ANCHOR}
      aria-labelledby="remote-play-heading"
      className="scroll-mt-4 space-y-4"
    >
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 id="remote-play-heading" className="flex items-center gap-2 text-base font-semibold">
            <MonitorPlay size={16} aria-hidden />
            {tr("remote_play", undefined, "Remote Play")}
          </h2>
          <p className="mt-1 text-sm text-[var(--color-muted)]">
            {tr(
              "remotePlay_subtitle_v2",
              undefined,
              "Set this PS5 up for Remote Play and get the PIN that pairs the PS Remote Play app with it.",
            )}
          </p>
        </div>
        <div className="flex shrink-0 gap-2">
        <Button variant="secondary" size="sm" leftIcon={<Gamepad2 size={14} />} onClick={() => navigate("/play")}>
          {tr("play_here", undefined, "Play Here")}
        </Button>
        <Button
          variant="ghost"
          size="sm"
          onClick={() => {
            void refresh();
            void refreshReadiness();
          }}
          disabled={busy || payloadStatus !== "up" || !addr}
          aria-label={tr("refresh", undefined, "Refresh")}
        >
          <RefreshCw size={14} />
        </Button>
        </div>
      </div>

        {error && <ErrorCard title={error} onRetry={() => void refresh()} />}

        {/* Setting a console up for Play is one checklist now (Play › Set up). */}
        <Callout
          tone="info"
          title={tr("play_setup_from_accounts", undefined, "Setting up Play? Play › Set up walks through every step and ticks each one by itself.")}
          action={
            <Button size="sm" variant="primary" onClick={() => navigate("/play/setup")} data-testid="accounts-play-setup">
              {tr("play_setup_title", undefined, "Set up Play")}
            </Button>
          }
        />

        {signIn && (
          <div id={SIGN_IN_ANCHOR} className="scroll-mt-4">
            {signIn}
          </div>
        )}

        {/* Request form */}
        <div className="surface-panel p-5">
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
                // The id pairing uses when the field is left empty: the signed-in account's.
                placeholder={
                  accountIdToHex(readiness?.account_id_b64 ?? "") ||
                  tr("remotePlay_account_id_placeholder_v2", undefined, "Signed-in account (automatic)")
                }
                className="w-full rounded-full border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-3 py-2 text-sm text-[var(--color-text)] placeholder:text-[var(--color-muted)] focus:border-[var(--color-accent)] focus:outline-none"
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
          <div className="surface-panel p-5">
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
              <div className="mt-3 rounded-[var(--radius-card)] border border-[var(--color-bad)] bg-[var(--color-bad-soft)] p-3">
                <p className="text-xs text-[var(--color-bad)]">
                  {status.err}
                </p>
              </div>
            )}
          </div>
        )}
    </section>
  );
}
