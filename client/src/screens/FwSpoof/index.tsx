import { useCallback, useEffect, useState } from "react";
import { ShieldAlert, RefreshCw, CheckCircle2, AlertTriangle, Info } from "lucide-react";
import { PageHeader, Button, ErrorCard, ConnectionGate, Card, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { fwSpoofStatus, type FwSpoofStatusResponse } from "../../api/ps5";

export default function FwSpoofScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";

  const [status, setStatus] = useState<FwSpoofStatusResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      const resp = await fwSpoofStatus(addr);
      setStatus(resp);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, payloadStatus]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const kernelUnknown = status?.kernel_release === "unknown" || !status?.kernel_release;
  const fwUnknown = status?.system_sw_version === "unknown";
  const swVersionDisplay = fwUnknown ? tr("fw_spoof_unknown", "Unknown") : (status?.system_sw_version || "—");
  const swParts = (!fwUnknown && swVersionDisplay !== "—") ? swVersionDisplay.split(".").map(Number) : [];

  return (
    <div className="p-6">
      <ConnectionGate>
        <PageHeader
          icon={ShieldAlert}
          title={tr("fw_spoof_title", undefined, "Firmware Spoof Detection")}
          description={tr(
            "fw_spoof_subtitle",
            undefined,
            "Detect if the reported firmware version has been modified",
          )}
          right={
            <Button variant="ghost" onClick={() => void refresh()} disabled={loading}>
              {loading ? <Spinner size={16} tone="inherit" /> : <RefreshCw size={16} />}
              {tr("refresh", undefined, "Refresh")}
            </Button>
          }
        />

        {error && <div className="mb-4"><ErrorCard title={error} /></div>}

        {loading && !status ? (
          <div className="flex items-center justify-center py-12">
            <Spinner size={32} />
          </div>
        ) : status ? (
          <>
            <Card
              className="mb-4 flex items-center gap-4"
              accent={status.spoofed}
            >
              {status.spoofed ? (
                <AlertTriangle size={32} className="shrink-0 text-[var(--color-bad)]" />
              ) : (
                <CheckCircle2 size={32} className="shrink-0 text-[var(--color-good)]" />
              )}
              <div className="min-w-0">
                <div className="text-lg font-semibold">
                  {status.spoofed
                    ? tr("fw_spoof_detected", undefined, "Spoofing Detected")
                    : tr("fw_spoof_clean", undefined, "No Spoofing Detected")}
                </div>
                <div className="text-sm text-[var(--color-muted)]">
                  {status.spoofed
                    ? tr(
                        "fw_spoof_detected_desc",
                        undefined,
                        "The reported firmware version doesn't match the kernel version.",
                      )
                    : tr(
                        "fw_spoof_clean_desc",
                        undefined,
                        "The reported firmware version appears genuine.",
                      )}
                </div>
              </div>
            </Card>

            {kernelUnknown && !status.spoofed && (
              <Card className="mb-4 flex items-center gap-3">
                <Info size={20} className="shrink-0 text-[var(--color-warn)]" />
                <div className="text-sm text-[var(--color-muted)]">
                  {tr(
                    "fw_spoof_kernel_unknown_warn",
                    undefined,
                    "Kernel version could not be read — spoofing detection may be unreliable.",
                  )}
                </div>
              </Card>
            )}

            <Card className="space-y-3">
              <h3 className="mb-1 font-semibold">
                {tr("fw_spoof_details", undefined, "Details")}
              </h3>
              <div className="grid grid-cols-1 gap-3 text-sm md:grid-cols-2">
                <div className="flex items-center justify-between gap-2 rounded-md bg-[var(--color-surface-3)] px-3 py-2">
                  <span className="text-[var(--color-muted)]">
                    {tr("fw_spoof_sw_version", undefined, "System SW Version")}
                  </span>
                  <code className="font-mono font-semibold">{swVersionDisplay}</code>
                </div>
                <div className="flex items-center justify-between gap-2 rounded-md bg-[var(--color-surface-3)] px-3 py-2">
                  <span className="text-[var(--color-muted)]">
                    {tr("fw_spoof_sw_raw", undefined, "Raw SW Value")}
                  </span>
                  <code className="font-mono">{status.system_sw_raw || "—"}</code>
                </div>
                <div className="flex items-center justify-between gap-2 rounded-md bg-[var(--color-surface-3)] px-3 py-2">
                  <span className="text-[var(--color-muted)]">
                    {tr("fw_spoof_kernel", undefined, "Kernel Release")}
                  </span>
                  <code className="font-mono">{status.kernel_release || "—"}</code>
                </div>
                <div className="flex items-center justify-between gap-2 rounded-md bg-[var(--color-surface-3)] px-3 py-2">
                  <span className="text-[var(--color-muted)]">
                    {tr("fw_spoof_kernel_fw", undefined, "Kernel FW Version")}
                  </span>
                  <code className="font-mono">{status.kernel_fw_version || "—"}</code>
                </div>
                {status.kernel_version && (
                  <div className="flex items-center justify-between gap-2 rounded-md bg-[var(--color-surface-3)] px-3 py-2 md:col-span-2">
                    <span className="text-[var(--color-muted)]">
                      {tr("fw_spoof_kern_version", undefined, "Kernel Build")}
                    </span>
                    <code className="font-mono text-xs">{status.kernel_version}</code>
                  </div>
                )}
                <div className="flex items-center justify-between gap-2 rounded-md bg-[var(--color-surface-3)] px-3 py-2">
                  <span className="text-[var(--color-muted)]">
                    {tr("fw_spoof_flag", undefined, "Spoofed")}
                  </span>
                  <span className={`font-semibold ${status.spoofed ? "text-[var(--color-bad)]" : "text-[var(--color-good)]"}`}>
                    {status.spoofed ? tr("fw_spoof_yes", "Yes") : tr("fw_spoof_no", "No")}
                  </span>
                </div>
              </div>
              {swParts.length >= 2 && swVersionDisplay !== "unknown" && (
                <div className="pt-1 text-xs text-[var(--color-muted)]">
                  {tr("fw_spoof_version_note", undefined, "Major")}: {swParts[0]} ·{" "}
                  {tr("fw_spoof_minor", undefined, "Minor")}: {swParts[1]}
                  {swParts.length >= 3 ? ` · ${tr("fw_spoof_rev", undefined, "Revision")}: ${swParts[2]}` : ""}
                </div>
              )}
            </Card>
          </>
        ) : null}
      </ConnectionGate>
    </div>
  );
}
