import { useState } from "react";

import { hostNetAllowFirewall, hostNetOpenSettings, type NetDiag } from "../../api/ps5";
import { Button } from "../../components";
// Direct import to avoid the barrel's circular-dep warning at build.
import { useConfirm } from "../../components/ConfirmDialog";
import { networkFixOffers } from "../../lib/networkFix";
import { useTr } from "../../state/lang";

/** The two fixes for "the console cannot reach this computer" on Windows (F2.1). Neither runs
 *  by itself: the first only opens Windows Settings, the second asks first and then Windows
 *  shows its own administrator prompt. */
export function NetworkFixActions({ diag }: { diag: NetDiag }) {
  const tr = useTr();
  const { confirm, dialog } = useConfirm();
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const offers = networkFixOffers(diag);
  if (!offers.makePrivate && !offers.allowProfile) return null;
  const network =
    offers.allowProfile === "private"
      ? tr("net_fix_private", undefined, "Private")
      : tr("net_fix_public", undefined, "Public");

  const openSettings = async () => {
    setNote(null);
    try {
      await hostNetOpenSettings(diag.adapter);
      setNote({
        ok: true,
        text: tr(
          "net_fix_settings_opened",
          { adapter: diag.adapter },
          "Windows Settings is open. Choose “{adapter}” and set its network profile to Private, then install again.",
        ),
      });
    } catch (e) {
      setNote({ ok: false, text: e instanceof Error ? e.message : String(e) });
    }
  };

  const allow = async () => {
    if (!offers.allowProfile) return;
    const ok = await confirm({
      title: tr(
        "net_fix_allow_title",
        { network },
        "Allow ps5upload on {network} networks?",
      ),
      message: tr(
        "net_fix_allow_body",
        { network },
        "Windows will ask for administrator permission. ps5upload will then be allowed to accept connections on {network} networks, which is how the PS5 reaches this computer. The rule covers ps5upload only.",
      ),
      confirmLabel: tr("net_fix_allow_confirm", undefined, "Continue"),
    });
    if (!ok) return;
    setBusy(true);
    setNote(null);
    try {
      await hostNetAllowFirewall(offers.allowProfile);
      setNote({
        ok: true,
        text: tr(
          "net_fix_allowed",
          { network },
          "ps5upload is now allowed on {network} networks. Install again.",
        ),
      });
    } catch (e) {
      setNote({ ok: false, text: e instanceof Error ? e.message : String(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mt-1 flex flex-col gap-1.5">
      {dialog}
      <div className="flex flex-wrap items-center gap-1.5">
        {offers.makePrivate && (
          <Button variant="secondary" size="sm" onClick={() => void openSettings()}>
            {tr("net_fix_make_private", undefined, "Make this network Private")}
          </Button>
        )}
        {offers.allowProfile && (
          <Button variant="secondary" size="sm" onClick={() => void allow()} disabled={busy} loading={busy}>
            {tr(
              "net_fix_allow_button",
              { network },
              "Allow ps5upload on {network} networks",
            )}
          </Button>
        )}
      </div>
      {note && (
        <div
          className={`text-xs ${note.ok ? "text-[var(--color-muted)]" : "text-[var(--color-bad)]"}`}
          role="status"
        >
          {note.text}
        </div>
      )}
    </div>
  );
}
