import { useCallback, useEffect, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";

import { Badge, Button, Input, Toggle } from "../../components";
import { formatBytes } from "../../lib/format";
import { humanizeCaught } from "../../lib/humanizeError";
import { useTr, type Translator } from "../../state/lang";
import {
  isIpv4,
  linkLong,
  usbNetOp,
  usbNetStatus,
  UsbNetUnsupportedError,
  type UsbNetAddress,
  type UsbNetState,
  type UsbNetStatus,
} from "../../api/usbNet";
import { Section } from "./SettingsLayout";

/** Re-read while it is on: the adapter, its cable and its address come and go on their own. */
export const USBNET_POLL_MS = 3000;

/** The state in plain words (catalog key and English, written out so the i18n scripts see them). */
const STATE: Record<UsbNetState, { key: string; text: string }> = {
  off: { key: "usbnet_state_off", text: "Off" },
  none: { key: "usbnet_state_none", text: "No adapter plugged in" },
  paused: { key: "usbnet_state_paused", text: "Paused while the PS5 rests" },
  starting: { key: "usbnet_state_starting", text: "Starting…" },
  no_link: { key: "usbnet_state_no_link", text: "No cable connected" },
  no_address: { key: "usbnet_state_no_address", text: "Waiting for an address" },
  ready: { key: "usbnet_state_ready", text: "Connected" },
  failed: { key: "usbnet_state_failed", text: "Couldn't start the adapter — unplug it and plug it back in" },
  unsupported: { key: "usbnet_state_unsupported", text: "This adapter's chip revision isn't supported yet" },
};

export function usbNetStateWords(tr: Translator, st: Pick<UsbNetStatus, "enabled" | "state">): string {
  const s = st.enabled ? (STATE[st.state] ?? STATE.starting) : STATE.off;
  return tr(s.key, undefined, s.text);
}

function sourceWords(tr: Translator, source: UsbNetStatus["source"]): string {
  if (source === "dhcp") return tr("usbnet_src_dhcp", undefined, "DHCP");
  if (source === "link-local") return tr("usbnet_src_link_local", undefined, "automatic, no DHCP server");
  if (source === "static") return tr("usbnet_src_static", undefined, "static");
  return "";
}

function stateTone(st: UsbNetStatus): string {
  if (!st.enabled) return "text-[var(--color-muted)]";
  if (st.state === "ready") return "text-[var(--color-good)]";
  if (st.state === "failed" || st.state === "unsupported") return "text-[var(--color-bad)]";
  return "text-[var(--color-text)]";
}

export interface UsbNetCardProps {
  /** null until the first answer. */
  st: UsbNetStatus | null;
  /** The helper predates USB adapters. */
  tooOld: boolean;
  busy: boolean;
  err: string;
  onToggle: (on: boolean) => void;
  onAddress: (opts: UsbNetAddress) => void;
}

/** Settings › This PS5 › USB Ethernet adapter: what the helper reports, the switch, and a static
 *  address for a network without a DHCP server. */
export function UsbNetCard({ st, tooOld, busy, err, onToggle, onAddress }: UsbNetCardProps) {
  const tr = useTr();
  const [staticOpen, setStaticOpen] = useState(false);
  const [ip, setIp] = useState("");
  const [mask, setMask] = useState("255.255.255.0");
  const [gateway, setGateway] = useState("");
  const [bad, setBad] = useState(false);

  const openStatic = () => {
    if (!staticOpen && st) {
      setIp(st.static_ip || "");
      if (st.source === "static") {
        if (st.mask) setMask(st.mask);
        setGateway(st.gateway || "");
      }
    }
    setBad(false);
    setStaticOpen(!staticOpen);
  };

  const save = () => {
    const ok = isIpv4(ip) && isIpv4(mask) && (gateway.trim() === "" || isIpv4(gateway));
    setBad(!ok);
    if (ok) onAddress({ ip: ip.trim(), mask: mask.trim(), gateway: gateway.trim() });
  };

  const chips = (
    <p className="text-xs text-[var(--color-muted)]">
      {tr("usbnet_supported", undefined, "Supported: Realtek RTL8156B (2.5G), RTL8157 (5G), RTL8159 (10G)")}
    </p>
  );
  const note = (
    <p className="text-xs text-[var(--color-muted)]">
      {tr(
        "usbnet_note",
        undefined,
        "The console's own network keeps working and is used whenever the adapter isn't.",
      )}
    </p>
  );

  let body: React.ReactNode;
  if (tooOld) {
    body = (
      <p className="text-sm text-[var(--color-muted)]" data-testid="usbnet-too-old">
        {tr("usbnet_too_old", undefined, "Update the PS5 helper to use a USB adapter.")}
      </p>
    );
  } else if (!st) {
    body = null;
  } else {
    const speed = linkLong(st.link_mbps);
    const adapterLine = st.adapter ? [st.adapter.chip || st.adapter.usb_id, speed].filter(Boolean).join(" · ") : "";
    const how = sourceWords(tr, st.source);
    const live = st.enabled && !!st.adapter && st.state !== "none" && st.state !== "off";
    const showSeen = st.enabled && (st.state === "none" || st.state === "unsupported") && (st.seen ?? []).length > 0;
    const route = st.enabled && st.state === "ready" ? st.route : undefined;
    body = (
      <>
        <Toggle
          checked={st.enabled}
          disabled={busy}
          onChange={onToggle}
          label={tr("usbnet_switch", undefined, "Use a USB Ethernet adapter")}
        />
        <div className="grid gap-1 text-sm">
          <div className={`font-medium ${stateTone(st)}`} data-testid="usbnet-state">
            {usbNetStateWords(tr, st)}
          </div>
          {live && adapterLine && (
            <div className="font-mono text-xs" data-testid="usbnet-adapter">
              {adapterLine}
            </div>
          )}
          {live && st.ip && (
            <div className="text-xs" data-testid="usbnet-ip">
              {how
                ? tr("usbnet_address", { ip: st.ip, how }, `Address: ${st.ip} (${how})`)
                : tr("usbnet_address_plain", { ip: st.ip }, `Address: ${st.ip}`)}
            </div>
          )}
          {live && (st.rx_bytes > 0 || st.tx_bytes > 0 || st.errors > 0) && (
            <div className="text-xs text-[var(--color-muted)]" data-testid="usbnet-traffic">
              {tr(
                "usbnet_traffic",
                { rx: formatBytes(st.rx_bytes), tx: formatBytes(st.tx_bytes), n: st.errors },
                `Received ${formatBytes(st.rx_bytes)} · Sent ${formatBytes(st.tx_bytes)} · Errors ${st.errors}`,
              )}
            </div>
          )}
          {route && (
            <div
              className={`text-xs ${route.preferred ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"}`}
              data-testid="usbnet-route"
            >
              {route.preferred
                ? tr("usbnet_route_used", undefined, "Transfers from this computer use the adapter")
                : route.checked_ms_ago === null
                  ? tr("usbnet_route_checking", undefined, "Checking whether this computer can reach the adapter…")
                  : tr(
                      "usbnet_route_unreachable",
                      undefined,
                      "This computer can't reach the adapter's address; transfers use the console network",
                    )}
            </div>
          )}
          {st.enabled && (st.state === "failed" || st.state === "unsupported") && st.error && (
            <div className="break-words text-xs text-[var(--color-muted)]">
              {tr("usbnet_error_detail", { err: st.error }, `Details: ${st.error}`)}
            </div>
          )}
          {showSeen && (
            <div className="break-words text-xs text-[var(--color-muted)]" data-testid="usbnet-seen">
              {tr("usbnet_seen", { ids: st.seen.join(", ") }, `Other USB devices: ${st.seen.join(", ")}`)}
            </div>
          )}
        </div>
        {st.enabled && (
          <div>
            <button
              type="button"
              className="flex items-center gap-1 text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]"
              aria-expanded={staticOpen}
              onClick={openStatic}
              data-testid="usbnet-static-toggle"
            >
              {staticOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} className="rtl:rotate-180" />}
              {tr("usbnet_static_toggle", undefined, "Static address")}
            </button>
            {staticOpen && (
              <div className="mt-2 grid gap-2" data-testid="usbnet-static">
                <p className="text-xs text-[var(--color-muted)]">
                  {tr("usbnet_static_hint", undefined, "For a network without a DHCP server.")}
                </p>
                <div className="grid gap-2 sm:grid-cols-3">
                  <Input
                    label={tr("usbnet_static_ip", undefined, "IP address")}
                    value={ip}
                    onChange={(e) => setIp(e.target.value)}
                    placeholder="192.168.1.50"
                    inputMode="decimal"
                    spellCheck={false}
                  />
                  <Input
                    label={tr("usbnet_static_mask", undefined, "Subnet mask")}
                    value={mask}
                    onChange={(e) => setMask(e.target.value)}
                    placeholder="255.255.255.0"
                    inputMode="decimal"
                    spellCheck={false}
                  />
                  <Input
                    label={tr("usbnet_static_gateway", undefined, "Gateway")}
                    value={gateway}
                    onChange={(e) => setGateway(e.target.value)}
                    placeholder="192.168.1.1"
                    inputMode="decimal"
                    spellCheck={false}
                  />
                </div>
                {bad && (
                  <p className="text-xs text-[var(--color-bad)]" role="alert">
                    {tr("usbnet_static_bad", undefined, "Enter addresses like 192.168.1.50.")}
                  </p>
                )}
                <div className="flex flex-wrap gap-2">
                  <Button size="sm" disabled={busy} onClick={save}>
                    {tr("usbnet_static_save", undefined, "Save")}
                  </Button>
                  {st.static_ip ? (
                    <Button size="sm" variant="secondary" disabled={busy} onClick={() => onAddress({ ip: "" })}>
                      {tr("usbnet_static_dhcp", undefined, "Use DHCP")}
                    </Button>
                  ) : null}
                </div>
              </div>
            )}
          </div>
        )}
      </>
    );
  }

  return (
    <Section
      title={tr("usbnet_title", undefined, "USB Ethernet adapter")}
      badge={
        <Badge tone="warn" className="normal-case tracking-normal">
          {tr("usbnet_experimental", undefined, "Experimental")}
        </Badge>
      }
      full
      testId="settings-usbnet"
    >
      <div className="grid gap-3">
        {body}
        {err && (
          <p className="text-xs text-[var(--color-bad)]" role="alert">
            {err}
          </p>
        )}
        {chips}
        {note}
      </div>
    </Section>
  );
}

/** The card for one console: reads the engine's view of the helper, and re-reads while it is on. */
export function UsbNetSettings({ host }: { host: string }) {
  const tr = useTr();
  const [st, setSt] = useState<UsbNetStatus | null>(null);
  const [tooOld, setTooOld] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const refresh = useCallback(async () => {
    try {
      setSt(await usbNetStatus(host));
      setTooOld(false);
    } catch (e) {
      if (e instanceof UsbNetUnsupportedError) setTooOld(true);
      // Any other failed read (the PS5 away) keeps what the card shows.
    }
  }, [host]);

  useEffect(() => {
    void refresh();
  }, [refresh]);
  useEffect(() => {
    if (!st?.enabled) return;
    const t = window.setInterval(() => {
      if (!document.hidden) void refresh();
    }, USBNET_POLL_MS);
    return () => window.clearInterval(t);
  }, [refresh, st?.enabled]);

  const run = async (op: "enable" | "disable" | "address", opts: UsbNetAddress = {}) => {
    setBusy(true);
    setErr("");
    try {
      setSt(await usbNetOp(host, op, opts));
    } catch (e) {
      if (e instanceof UsbNetUnsupportedError) setTooOld(true);
      else setErr(tr("usbnet_failed", { err: humanizeCaught(e) }, `Could not change the USB adapter on this PS5: ${humanizeCaught(e)}`));
    } finally {
      setBusy(false);
    }
  };

  return (
    <UsbNetCard
      st={st}
      tooOld={tooOld}
      busy={busy}
      err={err}
      onToggle={(on) => void run(on ? "enable" : "disable")}
      onAddress={(opts) => void run("address", opts)}
    />
  );
}
