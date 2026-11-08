import { useCallback, useEffect, useState } from "react";
import { ChevronDown, ChevronRight, ExternalLink, Loader2 } from "lucide-react";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import {
  appTailnetOp,
  appTailnetStatus,
  expiryDays,
  peerPathKind,
  type AppTailnetStatus,
} from "../../api/tailnet";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { QrCode } from "../Play/PhonePads";
import { humanizeCaught } from "../../lib/humanizeError";

/**
 * Settings › Tailscale: this device's own tailnet node, built into the app (no Tailscale app,
 * no VPN). Signed in, the app reaches a console that is on Tailscale ("Put this PS5 on
 * Tailscale" on the Connection screen) when its home address does not answer.
 */
export function TailscaleSettings() {
  const tr = useTr();
  const [st, setSt] = useState<AppTailnetStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [controlUrl, setControlUrl] = useState("");
  const [deviceName, setDeviceName] = useState("");

  const refresh = useCallback(async () => {
    try {
      setSt(await appTailnetStatus());
    } catch {
      /* an engine without Tailscale: the section stays at its first state */
    }
  }, []);

  useEffect(() => {
    void refresh();
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
        await appTailnetOp(
          op,
          op === "enable" ? { control_url: controlUrl.trim(), hostname: deviceName.trim() } : {},
        ),
      );
    } catch (e) {
      setErr(tr("tn_app_failed", { err: humanizeCaught(e) }, "Could not change Tailscale on this device: {err}"));
    } finally {
      setBusy(false);
    }
  };

  const state = st?.state ?? "off";
  const days = st ? expiryDays(st.key_expiry) : null;
  return (
    <div className="flex flex-col gap-2" data-testid="tailscale-settings">
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "tn_app_sub",
          undefined,
          "Reach your PS5 from anywhere. This device joins your tailnet by itself: no Tailscale app needed.",
        )}
      </p>
      {!st?.enabled ? (
        <div className="flex flex-col gap-2">
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
                  placeholder={st?.hostname || ""}
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
            <Button onClick={() => void run("enable")} disabled={busy} data-testid="app-tailnet-enable">
              {busy ? <Loader2 size={14} className="animate-spin" /> : null}
              {tr("tn_app_enable", undefined, "Sign in to Tailscale")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {state === "needs-login" && st.login_url ? (
            <div className="flex flex-col gap-2">
              <p className="text-xs">
                {tr("tn_app_sign_in", undefined, "Sign in to Tailscale to add this device to your tailnet.")}
              </p>
              <div>
                <Button onClick={() => void openExternalUrl(st.login_url)} data-testid="app-tailnet-login">
                  <ExternalLink size={14} />
                  {tr("tn_open_link", undefined, "Open sign-in page")}
                </Button>
              </div>
              <p className="text-xs text-[var(--color-muted)]">{tr("tn_scan", undefined, "Or scan this on your phone")}</p>
              <QrCode text={st.login_url} label={st.login_url} size={160} testId="app-tailnet-login-qr" />
            </div>
          ) : state === "running" && st.ip ? (
            <div className="flex flex-col gap-1">
              <p className="text-xs" data-testid="app-tailnet-on-as">
                {tr(
                  "tn_app_on_as",
                  { name: st.name.replace(/\.$/, ""), ip: st.ip },
                  "This device is on Tailscale as {name} at {ip}",
                )}
              </p>
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "tn_app_away_note",
                  undefined,
                  "When a PS5 on Tailscale does not answer at home, the app reaches it through Tailscale by itself.",
                )}
              </p>
              {st.peers.length === 0 ? (
                <p className="text-xs text-[var(--color-muted)]">
                  {tr("tn_no_peers", undefined, "No other devices on your tailnet yet.")}
                </p>
              ) : (
                <ul className="flex flex-col gap-0.5 text-xs" data-testid="app-tailnet-peers">
                  {st.peers.map((p) => {
                    const kind = peerPathKind(p);
                    return (
                      <li key={p.ip} className="flex justify-between gap-2">
                        <span className="truncate">{p.name.split(".")[0] || p.ip}</span>
                        <span className="text-[var(--color-muted)]">
                          {kind === "direct"
                            ? tr("tn_path_direct", { ms: p.latency_ms >= 0 ? p.latency_ms.toFixed(1) : "?" }, "direct, {ms} ms")
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
              {tr("tn_app_wait_approval", undefined, "Waiting for your tailnet admin to approve this device.")}
            </p>
          ) : (
            <p className="flex items-center gap-1 text-xs text-[var(--color-muted)]">
              <Loader2 size={12} className="animate-spin" />
              {tr("tn_connecting", undefined, "Connecting to Tailscale…")}
            </p>
          )}
          {days !== null ? (
            <p className="text-xs text-[var(--color-warn)]">
              {tr(
                "tn_app_key_expires",
                { days },
                "This device's Tailscale key expires in {days} days. Sign out and in again to renew it.",
              )}
            </p>
          ) : null}
          <div className="flex gap-2">
            <Button variant="secondary" onClick={() => void run("disable")} disabled={busy}>
              {tr("tn_turn_off", undefined, "Turn off")}
            </Button>
            <Button variant="secondary" onClick={() => void run("logout")} disabled={busy} data-testid="app-tailnet-logout">
              {tr("tn_sign_out", undefined, "Sign out")}
            </Button>
          </div>
        </div>
      )}
      {err ? <p className="text-xs text-[var(--color-bad)]">{err}</p> : null}
    </div>
  );
}
