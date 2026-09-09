import { useCallback, useEffect, useState } from "react";
import { Layers, RefreshCw, Wrench, AlertTriangle, CheckCircle2, Undo2 } from "lucide-react";
import { PageHeader, Button, ErrorCard, ConnectionGate, EmptyState, Card, Spinner } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { sdkScan, sdkPatch, sdkRestore, type SdkTitle } from "../../api/ps5";
import { PS5_FIRMWARES, fwToSdkHex, sdkHexToFw } from "../../lib/sdkVersionHex";

export default function SdkChangerScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";

  const [titles, setTitles] = useState<SdkTitle[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [patchTitleId, setPatchTitleId] = useState<string | null>(null);
  // A firmware version, not a raw hex word. The field used to take hex
  // and defaulted to 0x09060000 — which is not a real firmware, because
  // versions are BCD (9.60 is 0x09600000). See lib/fwVersion.ts.
  // 4.00 is what every shipped backport carries — measured across five
  // working titles on hardware — and a 4.xx backport runs on every firmware
  // above it, so it is the safe default rather than the console's own version.
  const [targetFw, setTargetFw] = useState("4.00");
  // BestPig's libc.prx swap: helps some titles, breaks others. Off by default.
  const [patchLibc, setPatchLibc] = useState(false);
  const [patching, setPatching] = useState(false);
  const [patchResult, setPatchResult] = useState<{ ok: boolean; msg: string } | null>(null);
  const [restoreTitleId, setRestoreTitleId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setLoading(true);
    setError(null);
    try {
      const resp = await sdkScan(addr);
      setTitles(resp.titles ?? []);
    } catch (e) {
      setError(humanizePs5Error(String(e)));
    } finally {
      setLoading(false);
    }
  }, [addr, payloadStatus]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const handlePatch = async (titleId: string) => {
    if (!addr) return;
    setPatching(true);
    setPatchResult(null);
    try {
      const hex = fwToSdkHex(targetFw);
      if (!hex) {
        setPatchResult({
          ok: false,
          msg: tr("sdk_bad_fw", undefined, `"${targetFw}" is not a firmware version`),
        });
        return;
      }
      const resp = await sdkPatch(titleId, hex, addr, patchLibc);
      if (resp.ok) {
        // The payload reports how many sites it actually rewrote, so the
        // result is verifiable rather than asserted.
        const detail = resp.detail;
        setPatchResult({
          ok: true,
          msg: detail
            ? `${titleId} → ${targetFw} (${detail})`
            : tr("sdk_patch_ok", undefined, `Patched ${titleId} → ${targetFw}`),
        });
      } else {
        setPatchResult({ ok: false, msg: resp.error ?? "Patch failed" });
      }
    } catch (e) {
      setPatchResult({ ok: false, msg: humanizePs5Error(String(e)) });
    } finally {
      setPatching(false);
      setPatchTitleId(null);
    }
  };

  const handleRestore = async (titleId: string) => {
    if (!addr) return;
    setRestoreTitleId(titleId);
    setPatchResult(null);
    try {
      const resp = await sdkRestore(titleId, addr);
      if (resp.ok) {
        const count = resp.restored ?? 0;
        if (count > 0) {
          setPatchResult({
            ok: true,
            msg: tr(
              "sdk_restore_ok",
              { count: String(count), titleId },
              `Restored ${count} file(s) for ${titleId} from backup.`,
            ),
          });
        } else {
          setPatchResult({
            ok: false,
            msg: tr(
              "sdk_restore_no_backup",
              { titleId },
              `No backup files found for ${titleId}.`,
            ),
          });
        }
      } else {
        setPatchResult({ ok: false, msg: resp.error ?? "Restore failed" });
      }
    } catch (e) {
      setPatchResult({ ok: false, msg: humanizePs5Error(String(e)) });
    } finally {
      setRestoreTitleId(null);
    }
  };

  return (
    <div className="p-6">
      <ConnectionGate>
        <PageHeader
          icon={Layers}
          title={tr("sdk_changer_title", undefined, "SDK Version Changer")}
          description={tr(
            "sdk_changer_subtitle",
            undefined,
            "Lower the firmware a game demands, so it will launch on an older console",
          )}
          right={
            <Button variant="ghost" onClick={() => void refresh()} disabled={loading}>
              {loading ? <Spinner size={16} tone="inherit" /> : <RefreshCw size={16} />}
              {tr("refresh", undefined, "Refresh")}
            </Button>
          }
        />

        {error && <div className="mb-4"><ErrorCard title={error} /></div>}

        {patchResult && (
          <Card
            className={`mb-4 flex items-center gap-3 ${
              patchResult.ok
                ? "border-[var(--color-good)]/30 bg-[var(--color-good)]/5"
                : "border-[var(--color-bad)]/30 bg-[var(--color-bad)]/5"
            }`}
          >
            {patchResult.ok ? (
              <CheckCircle2 size={20} className="text-[var(--color-good)]" />
            ) : (
              <AlertTriangle size={20} className="text-[var(--color-bad)]" />
            )}
            <span className={patchResult.ok ? "text-[var(--color-good)]" : "text-[var(--color-bad)]"}>
              {patchResult.msg}
            </span>
          </Card>
        )}

        <Card className="mb-4">
          <div className="text-sm leading-relaxed">
            <p className="mb-2">
              {tr(
                "sdk_explain_what",
                undefined,
                "Every game records the minimum firmware it needs. If that is higher than your console's firmware, the system refuses to start it. This screen lowers that recorded value.",
              )}
            </p>
            <p className="mb-2">
              <strong>
                {tr("sdk_explain_half", undefined, "This is only half of a backport.")}
              </strong>{" "}
              {tr(
                "sdk_explain_half_text",
                undefined,
                "Lowering the version gets the system to agree to launch the game — it will then usually crash on functions your firmware does not have. To finish the job you also need the newer system libraries, which BackPork mounts into the game at launch (see the Payloads screen).",
              )}
            </p>
            <p>
              {tr(
                "sdk_explain_decrypted",
                undefined,
                "Only decrypted games can be patched. Retail titles installed from disc or Store are encrypted, and those are reported as signed SELFs and skipped rather than damaged.",
              )}
            </p>
          </div>
        </Card>

        <Card className="mb-4 border-[var(--color-warn)]/30 bg-[var(--color-warn)]/5">
          <div className="flex items-start gap-3">
            <AlertTriangle size={20} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />
            <div className="text-sm">
              <strong>{tr("sdk_warning", undefined, "Warning:")}</strong>{" "}
              {tr(
                "sdk_warning_text",
                undefined,
                "Patching rewrites game binaries and param.json in place. Originals are copied to .bak first, and Restore puts them back. Use at your own risk.",
              )}
            </div>
          </div>
        </Card>

        {loading ? (
          <div className="flex items-center justify-center py-12">
            <Spinner size={32} />
          </div>
        ) : titles.length === 0 ? (
          <EmptyState
            icon={Layers}
            title={tr("sdk_no_titles", undefined, "No titles found")}
            message={tr(
              "sdk_no_titles_desc",
              undefined,
              "No installed apps detected in /user/appmeta",
            )}
          />
        ) : (
          <div className="space-y-2">
            {titles.map((t) => (
              <Card key={t.title_id} className="p-4">
                <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                  <div className="min-w-0 flex-1">
                    <div className="font-mono font-semibold">{t.title_id}</div>
                    {t.name && <div className="text-sm text-[var(--color-muted)]">{t.name}</div>}
                    <div className="mt-1 flex flex-wrap gap-4 text-xs text-[var(--color-muted)]">
                      <span>
                        {tr("sdk_sdk", undefined, "Built with SDK")}:{" "}
                        <code className="font-mono">
                          {sdkHexToFw(t.sdk_version ?? "") ?? t.sdk_version ?? "—"}
                        </code>
                      </span>
                      <span>
                        {tr("sdk_fw", undefined, "Needs firmware")}:{" "}
                        <code className="font-mono">
                          {sdkHexToFw(t.fw_required ?? "") ?? t.fw_required ?? "—"}
                        </code>
                      </span>
                    </div>
                    <div className="mt-1 truncate text-xs text-[var(--color-muted)]">
                      {t.patchable ? (
                        <>
                          {tr("sdk_source", undefined, "Source")}: {t.source}
                        </>
                      ) : (
                        tr(
                          "sdk_pkg_unpatchable",
                          undefined,
                          "Package-installed or unavailable source — cannot safely patch in place",
                        )
                      )}
                    </div>
                  </div>
                  {patchTitleId === t.title_id ? (
                    <div className="flex flex-wrap items-center gap-2">
                      <select
                        value={targetFw}
                        onChange={(e) => setTargetFw(e.target.value)}
                        className="input input-sm w-36 font-mono"
                        aria-label={tr(
                          "sdk_target_fw",
                          undefined,
                          "Target firmware",
                        )}
                      >
                        {PS5_FIRMWARES.map((fw) => (
                          <option key={fw} value={fw}>
                            {fw}
                          </option>
                        ))}
                      </select>
                      <label className="flex items-center gap-1.5 text-xs">
                        <input
                          type="checkbox"
                          checked={patchLibc}
                          onChange={(e) => setPatchLibc(e.target.checked)}
                        />
                        {tr(
                          "sdk_patch_libc",
                          undefined,
                          "Also patch libc.prx",
                        )}
                      </label>
                      <Button
                        variant="primary"
                        size="sm"
                        onClick={() => void handlePatch(t.title_id)}
                        disabled={patching}
                      >
                        {patching ? (
                          <Spinner size={14} tone="inherit" />
                        ) : (
                          tr("sdk_confirm", undefined, "Confirm")
                        )}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => setPatchTitleId(null)}
                      >
                        {tr("cancel", undefined, "Cancel")}
                      </Button>
                    </div>
                  ) : (
                    <div className="flex items-center gap-2">
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => {
                          setPatchTitleId(t.title_id);
                          setPatchResult(null);
                        }}
                        disabled={!t.patchable}
                        title={
                          t.patchable
                            ? tr("sdk_patch", undefined, "Patch")
                            : tr(
                                "sdk_pkg_unpatchable",
                                undefined,
                                "Package-installed or unavailable source — cannot safely patch in place",
                              )
                        }
                      >
                        <Wrench size={14} /> {tr("sdk_patch", undefined, "Patch")}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => void handleRestore(t.title_id)}
                        disabled={restoreTitleId !== null}
                        title={tr("sdk_restore_tooltip", undefined, "Restore original files from .bak backups")}
                      >
                        {restoreTitleId === t.title_id ? (
                          <Spinner size={14} tone="inherit" />
                        ) : (
                          <Undo2 size={14} />
                        )}
                        {tr("sdk_restore", undefined, "Restore")}
                      </Button>
                    </div>
                  )}
                </div>
              </Card>
            ))}
          </div>
        )}
      </ConnectionGate>
    </div>
  );
}
