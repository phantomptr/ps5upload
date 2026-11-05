import { useCallback, useEffect, useRef, useState } from "react";
import { useLocation } from "react-router";
import { AlertTriangle, CheckCircle2, HelpCircle, Info, RefreshCw, ShieldCheck } from "lucide-react";
import { Button, ErrorCard, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { fwSpoofStatus, type FwSpoofStatusResponse } from "../../api/ps5";
import { friendlyFw, fwSpoofVerdict } from "./fwSpoofVerdict";

/** The anchor old /fw-spoof links land on: /console#fw-spoof scrolls here. */
export const FW_SPOOF_ANCHOR = "fw-spoof";

/** Firmware spoof check, as a card on the Console page (it was its own screen until 8.5).
 *  Says in plain words what spoofing is, whether this console's reported firmware is spoofed,
 *  and what to do; the raw values the check read sit under "Technical details".
 *  The parent keys it on the host, so a console switch starts from a clean slate. */
export function FwSpoofCard({ host, payloadUp }: { host: string; payloadUp: boolean }) {
  const addr = host ? transferAddr(host) : "";
  const [status, setStatus] = useState<FwSpoofStatusResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!addr || !payloadUp) return;
    setLoading(true);
    setError(null);
    try {
      setStatus(await fwSpoofStatus(addr));
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, payloadUp]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // An old /fw-spoof link (sidebar favourite, bookmark) lands on /console#fw-spoof: bring the card
  // into view once.
  const { hash } = useLocation();
  const ref = useRef<HTMLElement>(null);
  const scrolled = useRef(false);
  useEffect(() => {
    if (hash !== `#${FW_SPOOF_ANCHOR}` || scrolled.current) return;
    scrolled.current = true;
    ref.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  }, [hash]);

  return (
    <FwSpoofCardView
      sectionRef={ref}
      status={status}
      loading={loading}
      error={error}
      canRefresh={payloadUp}
      onRefresh={() => void refresh()}
    />
  );
}

/** The card itself, from what the check returned; split out so it renders without a console. */
export function FwSpoofCardView({
  sectionRef,
  status,
  loading,
  error,
  canRefresh,
  onRefresh,
}: {
  sectionRef?: React.Ref<HTMLElement>;
  status: FwSpoofStatusResponse | null;
  loading: boolean;
  error: string | null;
  canRefresh: boolean;
  onRefresh: () => void;
}) {
  const tr = useTr();
  const verdict = status ? fwSpoofVerdict(status) : null;
  const fwUnknown = !status?.system_sw_version || status.system_sw_version === "unknown";
  const fw = status && !fwUnknown ? friendlyFw(status.system_sw_version) : "";
  const kernelUnknown = !status?.kernel_release || status.kernel_release === "unknown";
  const swParts = fwUnknown ? [] : status!.system_sw_version.split(".").map(Number);

  const tone =
    verdict === "spoofed" ? "--color-bad" : verdict === "unknown" ? "--color-warn" : "--color-good";
  const VerdictIcon = verdict === "spoofed" ? AlertTriangle : verdict === "unknown" ? HelpCircle : CheckCircle2;

  return (
    <section
      ref={sectionRef}
      id={FW_SPOOF_ANCHOR}
      className="surface-panel min-w-0 scroll-mt-4 p-5 sm:p-6"
      data-testid="fw-spoof-card"
    >
      <header className="mb-3 flex items-center gap-2.5 text-sm font-semibold">
        <span className="icon-disc" aria-hidden>
          <ShieldCheck size={14} />
        </span>
        <span className="min-w-0 flex-1">{tr("fw_spoof_title_v2", undefined, "Firmware spoof check")}</span>
        <Button
          variant="ghost"
          size="sm"
          onClick={onRefresh}
          disabled={loading || !canRefresh}
          aria-label={tr("refresh", undefined, "Refresh")}
          title={tr("refresh", undefined, "Refresh")}
        >
          {loading ? <Spinner size={12} tone="inherit" /> : <RefreshCw size={12} />}
        </Button>
      </header>

      <p className="mb-4 text-xs leading-relaxed text-[var(--color-muted)]">
        {tr(
          "fw_spoof_what_is",
          undefined,
          "Some payloads make a PS5 report a different firmware version than the one it really runs (spoofing), usually so newer games or updates will install. This checks that the version this console reports is genuine.",
        )}
      </p>

      {error && <ErrorCard title={error} onRetry={onRefresh} />}

      {loading && !status ? (
        <div className="flex justify-center py-6">
          <Spinner size={20} />
        </div>
      ) : status && verdict ? (
        <>
          <div
            className="flex items-start gap-3 rounded-[var(--radius-field)] p-3"
            style={{ background: `var(${tone}-soft)` }}
            role="status"
          >
            <VerdictIcon size={22} className="mt-0.5 shrink-0" style={{ color: `var(${tone})` }} aria-hidden />
            <div className="min-w-0 text-sm">
              <div className="font-semibold">
                {verdict === "spoofed"
                  ? tr("fw_spoof_verdict_spoofed", undefined, "Spoofed: the reported firmware isn't real")
                  : verdict === "unknown"
                    ? tr("fw_spoof_verdict_unknown", undefined, "Can't tell")
                    : tr("fw_spoof_verdict_clean", { fw }, `Not spoofed: firmware ${fw} is genuine`)}
              </div>
              <p className="mt-1 text-[var(--color-muted)]">
                {verdict === "spoofed"
                  ? tr(
                      "fw_spoof_todo_spoofed",
                      undefined,
                      "Don't choose payloads, exploits or games by the version this console shows. Restart it without the spoofing payload, then check again.",
                    )
                  : verdict === "unknown"
                    ? tr(
                        "fw_spoof_todo_unknown",
                        undefined,
                        "This console's kernel didn't give up its firmware version, so nothing could be checked. Load a payload with kernel access (kstuff or etaHEN), then check again.",
                      )
                    : tr("fw_spoof_todo_clean", undefined, "Nothing to do.")}
              </p>
            </div>
          </div>

          {kernelUnknown && verdict === "clean" && (
            <p className="mt-3 flex items-start gap-2 text-xs text-[var(--color-muted)]">
              <Info size={14} className="mt-px shrink-0 text-[var(--color-warn)]" aria-hidden />
              {tr(
                "fw_spoof_kernel_unknown_warn",
                undefined,
                "Kernel version could not be read — spoofing detection may be unreliable.",
              )}
            </p>
          )}

          <details className="mt-3 text-xs">
            <summary className="cursor-pointer select-none text-[var(--color-muted)] hover:text-[var(--color-text)]">
              {tr("fw_spoof_technical", undefined, "Technical details")}
            </summary>
            <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1.5">
              <DetailRow
                label={tr("fw_spoof_sw_version", undefined, "System SW Version")}
                value={fwUnknown ? tr("fw_spoof_unknown", "Unknown") : status.system_sw_version}
              />
              <DetailRow label={tr("fw_spoof_sw_raw", undefined, "Raw SW Value")} value={status.system_sw_raw} />
              <DetailRow label={tr("fw_spoof_kernel", undefined, "Kernel Release")} value={status.kernel_release} />
              <DetailRow
                label={tr("fw_spoof_kernel_fw", undefined, "Kernel FW Version")}
                value={status.kernel_fw_version}
              />
              {status.kernel_version && (
                <DetailRow label={tr("fw_spoof_kern_version", undefined, "Kernel Build")} value={status.kernel_version} />
              )}
              <DetailRow
                label={tr("fw_spoof_flag", undefined, "Spoofed")}
                value={status.spoofed ? tr("fw_spoof_yes", "Yes") : tr("fw_spoof_no", "No")}
              />
            </dl>
            {swParts.length >= 2 && (
              <p className="mt-2 text-[var(--color-muted)]">
                {tr("fw_spoof_version_note", undefined, "Major")}: {swParts[0]} ·{" "}
                {tr("fw_spoof_minor", undefined, "Minor")}: {swParts[1]}
                {swParts.length >= 3 ? ` · ${tr("fw_spoof_rev", undefined, "Revision")}: ${swParts[2]}` : ""}
              </p>
            )}
          </details>
        </>
      ) : null}
    </section>
  );
}

function DetailRow({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-[var(--color-muted)]">{label}</dt>
      <dd className="min-w-0 break-words text-end font-mono">{value || "—"}</dd>
    </>
  );
}
