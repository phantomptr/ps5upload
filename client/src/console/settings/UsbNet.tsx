import { useCallback, useEffect, useState } from "react";
import { linkLong } from "../../lib/linkSpeed";
import { ApiError, call } from "../api";
import { t } from "../strings";
import { pollWhileVisible } from "../ui/visiblePoll";

/** What the helper's /api/usbnet answers (payload/include/usbnet.h), the part this page shows. */
export interface ConsoleUsbNet {
  enabled: boolean;
  state: string;
  adapter?: { usb_id: string; chip: string; max_mbps: number } | null;
  link_mbps?: number;
  ip?: string;
  source?: string;
  error?: string;
}

/** Re-read while it is on: the adapter, its cable and its address come and go on their own. */
export const USBNET_POLL_MS = 3000;

const STATE_KEYS: Record<string, string> = {
  none: "cw_usbnet_state_none",
  paused: "cw_usbnet_state_paused",
  starting: "cw_usbnet_state_starting",
  no_link: "cw_usbnet_state_no_link",
  no_address: "cw_usbnet_state_no_address",
  ready: "cw_usbnet_state_ready",
  failed: "cw_usbnet_state_failed",
  unsupported: "cw_usbnet_state_unsupported",
};

/** Where it stands, in words. */
export function usbNetLine(s: ConsoleUsbNet): string {
  if (!s.enabled || s.state === "off") return t("cw_usbnet_state_off");
  return t(STATE_KEYS[s.state] ?? "cw_usbnet_state_starting");
}

/** "RTL8156B · 2.5 Gbit/s" while an adapter is in use; null otherwise. */
export function usbNetAdapterLine(s: ConsoleUsbNet): string | null {
  if (!s.enabled || !s.adapter || s.state === "none" || s.state === "off") return null;
  return [s.adapter.chip || s.adapter.usb_id, linkLong(s.link_mbps)].filter(Boolean).join(" · ");
}

export interface UsbNetSectionViewProps {
  st: ConsoleUsbNet;
  busy: boolean;
  err: string | null;
  onToggle: () => void;
}

export function UsbNetSectionView({ st, busy, err, onToggle }: UsbNetSectionViewProps) {
  const on = !!st.enabled;
  const adapter = usbNetAdapterLine(st);
  const bad = on && (st.state === "failed" || st.state === "unsupported");
  return (
    <section className="cw-home-section" data-setting="usbnet">
      <h2 className="cw-h2">
        {t("cw_usbnet_title")}
        <span className="cw-tag">{t("cw_usbnet_experimental")}</span>
      </h2>
      <div className="cw-setting">
        <button
          type="button"
          role="switch"
          aria-checked={on}
          disabled={busy}
          className="cw-focusable cw-button"
          data-action="usbnet"
          onClick={onToggle}
        >
          <span>{t("cw_usbnet_switch")}</span>
          <span className={`cw-tag${on ? " is-on" : ""}`}>{t(on ? "cw_settings_on" : "cw_settings_off")}</span>
        </button>
        <p className={bad ? "cw-msg is-error" : "cw-dim cw-setting-note"} data-usbnet-line="" role={bad ? "alert" : undefined}>
          {usbNetLine(st)}
        </p>
        {adapter ? (
          <p className="cw-dim cw-setting-note cw-mono" data-usbnet-adapter="">
            {adapter}
          </p>
        ) : null}
        {adapter && st.ip ? (
          <p className="cw-dim cw-setting-note" data-usbnet-ip="">
            {t("cw_usbnet_address", { ip: st.ip })}
          </p>
        ) : null}
      </div>
      <p className="cw-dim cw-setting-note">{t("cw_usbnet_note")}</p>
      <p className="cw-dim cw-setting-note" data-usbnet-risk="">
        {t("cw_usbnet_risk")}
      </p>
      {err ? (
        <div className="cw-msg is-error" role="alert">
          {err}
        </div>
      ) : null}
    </section>
  );
}

/**
 * Settings › USB Ethernet adapter on the console page (experimental): the same switch as the app's
 * Settings › This PS5, for someone in front of the PS5. Hidden on a helper without the route.
 */
export function UsbNetSection() {
  const [st, setSt] = useState<ConsoleUsbNet | null>(null);
  const [missing, setMissing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const read = useCallback(async () => {
    try {
      setSt(await call<ConsoleUsbNet>("/api/usbnet", { op: "status" }));
      setMissing(false);
    } catch (e) {
      // An older helper has no /api/usbnet (404): no section. Any other failed read keeps what it shows.
      if (e instanceof ApiError && e.status === 404) setMissing(true);
    }
  }, []);

  useEffect(() => {
    void read();
  }, [read]);
  useEffect(() => {
    if (!st?.enabled) return;
    return pollWhileVisible(read, USBNET_POLL_MS);
  }, [st?.enabled, read]);

  const toggle = async () => {
    if (!st) return;
    setBusy(true);
    setErr(null);
    try {
      setSt(await call<ConsoleUsbNet>("/api/usbnet", { op: st.enabled ? "disable" : "enable" }));
    } catch {
      setErr(t("cw_usbnet_err_change"));
    } finally {
      setBusy(false);
    }
  };

  if (!st || missing) return null;
  return <UsbNetSectionView st={st} busy={busy} err={err} onToggle={() => void toggle()} />;
}
