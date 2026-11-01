import { useEffect, useState } from "react";

import { remoteplayReadiness } from "../../../api/ps5";
import { Button, Callout } from "../../../components";
import { transferAddr } from "../../../lib/addr";
import { humanizePs5Error } from "../../../lib/humanizeError";
import { pairConsole } from "../../../lib/pairConsole";
import { accountIdToHex, accountIdToPairingB64 } from "../../../lib/remoteplay";
import { useTr } from "../../../state/lang";

/** The PIN as typed: its digits only, at most the eight the PS5 shows. */
export function pinDigits(raw: string): string {
  return raw.replace(/\D/g, "").slice(0, 8);
}

/** Ready to pair: eight digits and a readable account id. */
export function canPairWithPin(pin: string, accountId: string): boolean {
  return pin.length === 8 && accountIdToPairingB64(accountId) !== null;
}

/**
 * Pairing with the PIN the PS5 itself shows (#609), the way PS Remote Play and chiaki-ng pair:
 * Settings › System › Remote Play › Link Device shows eight digits, the user types them here and
 * the engine registers with them. No helper is involved, so it works on any firmware, also where
 * the automatic pairing cannot make a PIN. The account id is the signed-in user's when the helper
 * can read it, else typed (0x… hex, decimal or the base64 chiaki-ng shows).
 */
export function PinPairForm({
  host,
  profileId,
  accountIdB64,
  onPaired,
}: {
  host: string;
  profileId: string;
  /** The account the console would pair, when known (the helper's readiness). */
  accountIdB64?: string | null;
  onPaired?: (nickname: string | null) => void;
}) {
  const tr = useTr();
  const [pin, setPin] = useState("");
  const [account, setAccount] = useState(() => accountIdToHex(accountIdB64 ?? ""));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  // No id handed in: ask the helper once, quietly (without it the user types the id).
  useEffect(() => {
    if (accountIdB64 || !host) return;
    let live = true;
    remoteplayReadiness(transferAddr(host))
      .then((r) => {
        const hex = accountIdToHex(r.account_id_b64 ?? "");
        if (live && hex) setAccount((a) => a || hex);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [host, accountIdB64]);

  const pair = async () => {
    const b64 = accountIdToPairingB64(account);
    if (pin.length !== 8 || !b64) return;
    setBusy(true);
    setError(null);
    try {
      const name = await pairConsole(host, profileId, undefined, { pin, accountIdB64: b64 });
      setDone(true);
      setPin("");
      onPaired?.(name);
    } catch (e) {
      setError(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-2 text-sm" data-testid="pin-pair">
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_pin_pair_how",
          undefined,
          "On the PS5, open Settings › System › Remote Play › Link Device. Type the 8-digit number it shows, while it shows it.",
        )}
      </p>
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          void pair();
        }}
      >
        <label className="flex flex-col gap-1 text-xs text-[var(--color-muted)]">
          {tr("rp_pin_pair_pin", undefined, "PIN on the PS5")}
          <input
            className="input w-36 py-1 text-center font-mono tracking-widest"
            inputMode="numeric"
            autoComplete="off"
            maxLength={9}
            placeholder="12345678"
            value={pin}
            onChange={(e) => setPin(pinDigits(e.target.value))}
            data-testid="pin-pair-pin"
          />
        </label>
        <label className="flex min-w-0 flex-1 flex-col gap-1 text-xs text-[var(--color-muted)]">
          {tr("rp_pin_pair_account", undefined, "Account ID of the user you play as")}
          <input
            className="input py-1 font-mono"
            autoComplete="off"
            spellCheck={false}
            placeholder="0x…"
            value={account}
            onChange={(e) => setAccount(e.target.value)}
            data-testid="pin-pair-account"
          />
        </label>
        <Button
          type="submit"
          size="sm"
          variant="primary"
          loading={busy}
          disabled={busy || !canPairWithPin(pin, account)}
          data-testid="pin-pair-go"
        >
          {tr("play_setup_pair", undefined, "Pair")}
        </Button>
      </form>
      {account.trim() !== "" && accountIdToPairingB64(account) === null && (
        <p className="text-xs text-[var(--color-bad)]">
          {tr("rp_pin_pair_bad_account", undefined, "That is not an account ID. Accounts shows it for each user (0x…).")}
        </p>
      )}
      {error && <Callout tone="error" title={error} />}
      {done && !error && (
        <p className="text-xs text-[var(--color-good)]" data-testid="pin-pair-done">
          {tr("rp_pin_pair_done", undefined, "Paired.")}
        </p>
      )}
    </div>
  );
}
