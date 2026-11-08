// Small pieces shared by the PS5's Tailscale panel and Settings › Tailscale (this device).

import { useEffect, useState } from "react";
import { ExternalLink, RefreshCw } from "lucide-react";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { openExternalUrl } from "../../lib/openExternalUrl";

/** Tailscale's admin page, where a device's key expiry is turned off. */
export const TAILSCALE_ADMIN_MACHINES = "https://login.tailscale.com/admin/machines";

/** On / Paused, the same switch Home has for this device. Paused keeps the sign-in. */
export function OnPausedSwitch({
  on,
  connecting,
  busy,
  onChange,
  testId,
}: {
  on: boolean;
  connecting?: boolean;
  busy: boolean;
  onChange: (on: boolean) => void;
  testId?: string;
}) {
  const tr = useTr();
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      disabled={busy}
      data-testid={testId}
      onClick={() => onChange(!on)}
      className={`rounded-full border px-2.5 py-0.5 text-xs font-medium ${
        on
          ? "border-transparent bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]"
          : "border-[var(--glass-edge)] text-[var(--color-muted)]"
      }`}
    >
      {!on
        ? tr("v5_home_tailnet_paused", undefined, "Paused")
        : connecting
          ? tr("v5_home_tailnet_connecting", undefined, "Connecting…")
          : tr("v5_home_tailnet_on", undefined, "On")}
    </button>
  );
}

/** The device's name on Tailscale, editable any time (applied now while on; the address stays). */
export function NameField({
  current,
  placeholder,
  busy,
  onSave,
  testId,
}: {
  current: string;
  placeholder: string;
  busy: boolean;
  onSave: (name: string) => void;
  testId?: string;
}) {
  const tr = useTr();
  const [name, setName] = useState(current);
  useEffect(() => setName(current), [current]);
  const changed = name.trim() !== "" && name.trim() !== current;
  return (
    <div className="flex flex-wrap items-end gap-2" data-testid={testId}>
      <label className="flex min-w-[12rem] flex-1 flex-col gap-1 text-xs">
        {tr("tn_device_name", undefined, "Device name")}
        <input
          className="rounded border border-[var(--glass-edge)] bg-transparent px-2 py-1"
          value={name}
          placeholder={placeholder}
          maxLength={63}
          spellCheck={false}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <Button size="sm" variant="secondary" disabled={busy || !changed} onClick={() => onSave(name.trim())}>
        {tr("tn_rename", undefined, "Rename")}
      </Button>
    </div>
  );
}

/** "Renew": sign in again keeping this device (its address and name), then open the sign-in
 *  page from the status that follows. */
export function RenewButton({ busy, onRenew, testId }: { busy: boolean; onRenew: () => void; testId?: string }) {
  const tr = useTr();
  return (
    <Button size="sm" variant="secondary" leftIcon={<RefreshCw size={14} />} disabled={busy} onClick={onRenew} data-testid={testId}>
      {tr("tn_renew", undefined, "Renew")}
    </Button>
  );
}

/** The key-expiry hint for a PS5: turning expiry off in Tailscale's admin page ends renewals. */
export function KeyExpiryHint() {
  const tr = useTr();
  return (
    <p className="flex flex-wrap items-center gap-1 text-xs text-[var(--color-muted)]" data-testid="tailnet-expiry-hint">
      {tr(
        "tn_expiry_hint_ps5",
        undefined,
        "Tip: in Tailscale's admin page, open this PS5 and choose Disable key expiry, so it never has to sign in again.",
      )}
      <button
        type="button"
        className="inline-flex items-center gap-0.5 underline"
        onClick={() => void openExternalUrl(TAILSCALE_ADMIN_MACHINES)}
      >
        <ExternalLink size={12} />
        {tr("tn_open_admin", undefined, "Open the admin page")}
      </button>
    </p>
  );
}
