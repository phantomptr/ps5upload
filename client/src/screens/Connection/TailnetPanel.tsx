import { useCallback, useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ExternalLink, Globe, Loader2 } from "lucide-react";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import {
  expiryDays,
  peerPathKind,
  tailnetOp,
  tailnetStatus,
  TailnetUnsupportedError,
  type TailnetStatus,
} from "../../api/tailnet";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { QrCode } from "../Play/PhonePads";
import { humanizeCaught } from "../../lib/humanizeError";

/**
 * "Put this PS5 on Tailscale": the console joins the user's tailnet through the helper and
 * serves only Remote Play and PS5Upload to it. Sign-in is Tailscale's own page (a link and a
 * QR code for the phone); the advanced fields are for a Headscale server.
 */
export default function TailnetPanel({ host }: { host: string }) {
  const tr = useTr();
  const [st, setSt] = useState<TailnetStatus | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [controlUrl, setControlUrl] = useState("");
  const [deviceName, setDeviceName] = useState("");

  const refresh = useCallback(async () => {
    try {
      setSt(await tailnetStatus(host));
      setUnsupported(false);
    } catch (e) {
      if (e instanceof TailnetUnsupportedError) setUnsupported(true);
    }
  }, [host]);

  useEffect(() => {
    void refresh();
    // While it is on, follow sign-in and paths; off, one read is enough.
    const t = window.setInterval(() => {
      if (st?.enabled) void refresh();
    }, 3000);
    return () => window.clearInterval(t);
  }, [refresh, st?.enabled]);

  const run = async (op: "enable" | "disable" | "logout") => {
    setBusy(true);
    setErr("");
    try {
      setSt(
        await tailnetOp(
          host,
          op,
          op === "enable" ? { control_url: controlUrl.trim(), hostname: deviceName.trim() } : {},
        ),
      );
    } catch (e) {
      if (e instanceof TailnetUnsupportedError) setUnsupported(true);
      else setErr(tr("tn_failed", { err: humanizeCaught(e) }, "Could not change Tailscale on this PS5: {err}"));
    } finally {
      setBusy(false);
    }
  };

  const state = st?.state ?? "off";
  return (
    <section
      className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3"
      data-testid="tailnet-panel"
    >
      <div className="flex items-center gap-2 text-sm font-semibold">
        <Globe size={16} />
        {tr("tn_title", undefined, "Tailscale")}
      </div>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "tn_sub",
          undefined,
          "Play on this PS5 from anywhere through your own tailnet. Only Remote Play and PS5Upload are reachable.",
        )}
      </p>
      {unsupported ? (
        <p className="text-xs text-[var(--color-warn)]">
          {tr("tn_old_helper", undefined, "This PS5's helper is older than Tailscale support. Send the payload again.")}
        </p>
      ) : !st?.enabled ? (
        <div className="flex flex-col gap-2">
          {state === "signed-out" && (
            <p className="text-xs">{tr("tn_signed_out", undefined, "Signed out. Turn it on again to sign in.")}</p>
          )}
          <button
            type="button"
            className="flex items-center gap-1 text-start text-xs text-[var(--color-muted)] hover:text-[var(--color-fg)]"
            onClick={() => setAdvanced((v) => !v)}
          >
            {advanced ? <ChevronDown size={12} /> : <ChevronRight size={12} className="rtl:rotate-180" />}
            {tr("tn_advanced", undefined, "Advanced")}
          </button>
          {advanced && (
            <div className="flex flex-col gap-2">
              <label className="flex flex-col gap-1 text-xs">
                {tr("tn_device_name", undefined, "Device name")}
                <input
                  className="rounded border border-[var(--glass-edge)] bg-transparent px-2 py-1"
                  value={deviceName}
                  placeholder={st?.hostname || "PS5"}
                  onChange={(e) => setDeviceName(e.target.value)}
                />
              </label>
              <label className="flex flex-col gap-1 text-xs">
                {tr("tn_control_url", undefined, "Control server (for Headscale)")}
                <input
                  className="rounded border border-[var(--glass-edge)] bg-transparent px-2 py-1"
                  value={controlUrl}
                  placeholder={st?.control_url || "https://controlplane.tailscale.com"}
                  onChange={(e) => setControlUrl(e.target.value)}
                  spellCheck={false}
                />
              </label>
            </div>
          )}
          <div>
            <Button onClick={() => void run("enable")} disabled={busy} data-testid="tailnet-enable">
              {busy ? <Loader2 size={14} className="animate-spin" /> : null}
              {tr("tn_enable", undefined, "Put this PS5 on Tailscale")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {state === "needs-login" && st.login_url ? (
            <div className="flex flex-col gap-2">
              <p className="text-xs">
                {tr("tn_sign_in", undefined, "Sign in to Tailscale to add this PS5 to your tailnet.")}
              </p>
              <div>
                <Button onClick={() => void openExternalUrl(st.login_url)} data-testid="tailnet-login">
                  <ExternalLink size={14} />
                  {tr("tn_open_link", undefined, "Open sign-in page")}
                </Button>
              </div>
              <p className="text-xs text-[var(--color-muted)]">{tr("tn_scan", undefined, "Or scan this on your phone")}</p>
              <QrCode text={st.login_url} label={st.login_url} size={160} testId="tailnet-login-qr" />
            </div>
          ) : state === "running" && st.ip ? (
            <div className="flex flex-col gap-1">
              <p className="text-xs" data-testid="tailnet-on-as">
                {tr("tn_on_as", { name: st.name.replace(/\.$/, ""), ip: st.ip }, "On Tailscale as {name} at {ip}")}
              </p>
              {st.peers.length === 0 ? (
                <p className="text-xs text-[var(--color-muted)]">
                  {tr("tn_no_peers", undefined, "No other devices on your tailnet yet.")}
                </p>
              ) : (
                <ul className="flex flex-col gap-0.5 text-xs" data-testid="tailnet-peers">
                  {st.peers.map((p) => {
                    const kind = peerPathKind(p);
                    return (
                      <li key={p.ip} className="flex justify-between gap-2">
                        <span className="truncate">{p.name.split(".")[0] || p.ip}</span>
                        <span className="text-[var(--color-muted)]">
                          {kind === "direct"
                            ? tr("tn_path_direct", { ms: p.latency_ms.toFixed(1) }, "direct, {ms} ms")
                            : kind === "relayed"
                              ? tr("tn_path_relayed", undefined, "relayed through Tailscale's servers")
                              : tr("tn_path_idle", undefined, "idle")}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          ) : state === "wait-approval" ? (
            <p className="text-xs">
              {tr("tn_wait_approval", undefined, "Waiting for your tailnet admin to approve this PS5.")}
            </p>
          ) : (
            <p className="flex items-center gap-1 text-xs text-[var(--color-muted)]">
              <Loader2 size={12} className="animate-spin" />
              {tr("tn_connecting", undefined, "Connecting to Tailscale…")}
            </p>
          )}
          {expiryDays(st.key_expiry) !== null ? (
            <p className="text-xs text-[var(--color-warn)]" data-testid="tailnet-expiry">
              {tr(
                "tn_key_expires",
                { days: expiryDays(st.key_expiry) ?? 0 },
                "This PS5's Tailscale key expires in {days} days. Sign out and in again to renew it.",
              )}
            </p>
          ) : null}
          <div className="flex gap-2">
            <Button variant="secondary" onClick={() => void run("disable")} disabled={busy}>
              {tr("tn_turn_off", undefined, "Turn off")}
            </Button>
            <Button variant="secondary" onClick={() => void run("logout")} disabled={busy} data-testid="tailnet-logout">
              {tr("tn_sign_out", undefined, "Sign out")}
            </Button>
          </div>
        </div>
      )}
      {err ? <p className="text-xs text-[var(--color-bad)]">{err}</p> : null}
    </section>
  );
}
