import { useCallback, useEffect, useState } from "react";
import { CheckCircle2, ChevronDown, ChevronRight, Circle, ExternalLink, Globe, Loader2, Settings2 } from "lucide-react";
import { useNavigate } from "react-router";
import { Button } from "../../components";
import { useTr } from "../../state/lang";
import {
  appTailnetStatus,
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
import { TailnetProblems } from "./TailnetProblems";

/**
 * "Put this PS5 on Tailscale": the console joins the user's tailnet through the helper and
 * serves only Remote Play and PS5Upload to it. Sign-in is Tailscale's own page (a link and a
 * QR code for the phone); the advanced fields are for a Headscale server. The panel spells out
 * the two steps (this PS5 here, each playing device in Settings › Tailscale) and shows how far
 * along both are, since the two sign-ins were easy to confuse.
 */
export default function TailnetPanel({ host }: { host: string }) {
  const tr = useTr();
  const navigate = useNavigate();
  const [st, setSt] = useState<TailnetStatus | null>(null);
  /** This device's own node (Settings › Tailscale): step 2. null until read. */
  const [selfOn, setSelfOn] = useState<boolean | null>(null);
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
    try {
      const me = await appTailnetStatus();
      setSelfOn(me.enabled && me.state === "running");
    } catch {
      /* an engine without Tailscale: step 2 is just not shown as done */
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
  const psOn = !!st?.enabled && state === "running" && !!st.ip;
  return (
    <section
      className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3"
      data-testid="tailnet-panel"
    >
      <div className="flex items-center gap-2 text-sm font-semibold">
        <Globe size={16} />
        {tr("tn_title_ps5", undefined, "Tailscale for this PS5")}
      </div>
      <p className="text-xs text-[var(--color-muted)]">
        {tr("tn_sub", undefined, "Play this PS5 when you're away from home. Two steps, each done once:")}
      </p>
      <ol className="flex flex-col gap-1 text-xs" data-testid="tailnet-steps">
        <li className="flex items-start gap-1.5">
          {psOn ? (
            <CheckCircle2 size={14} className="mt-px shrink-0 text-[var(--color-good)]" />
          ) : (
            <Circle size={14} className="mt-px shrink-0 text-[var(--color-muted)]" />
          )}
          <span>
            <span className="font-semibold">1.</span>{" "}
            {tr("tn_step_ps5", undefined, "Here, at home: put this PS5 on your Tailscale network.")}
          </span>
        </li>
        <li className="flex items-start gap-1.5">
          {selfOn ? (
            <CheckCircle2 size={14} className="mt-px shrink-0 text-[var(--color-good)]" />
          ) : (
            <Circle size={14} className="mt-px shrink-0 text-[var(--color-muted)]" />
          )}
          <span>
            <span className="font-semibold">2.</span>{" "}
            {tr(
              "tn_step_devices",
              undefined,
              "On each phone or computer you'll play from: Settings › Tailscale › Sign in to Tailscale.",
            )}
          </span>
        </li>
      </ol>
      <p className="text-xs text-[var(--color-muted)]">
        {tr("tn_only_rp", undefined, "Only Remote Play and PS5Upload can reach the PS5 this way.")}
      </p>
      {unsupported ? (
        <p className="text-xs text-[var(--color-warn)]">
          {tr("tn_old_helper", undefined, "This PS5's helper is too old for Tailscale. Send the helper again.")}
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
                {tr("tn_sign_in", undefined, "Sign in with your Tailscale account to add this PS5 to your Tailscale network.")}
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
              {selfOn ? (
                <p className="text-xs text-[var(--color-good)]" data-testid="tailnet-self-on">
                  {tr("tn_this_device_on", undefined, "This device is on Tailscale too, so it can reach this PS5 from anywhere.")}
                </p>
              ) : (
                <div className="flex flex-wrap items-center gap-2">
                  <p className="text-xs" data-testid="tailnet-self-off">
                    {tr("tn_this_device_off", undefined, "Next: add the devices you'll play from. This one isn't on Tailscale yet.")}
                  </p>
                  <Button size="sm" variant="secondary" leftIcon={<Settings2 size={14} />} onClick={() => navigate("/settings#tailscale")}>
                    {tr("tn_open_settings", undefined, "Open Settings › Tailscale")}
                  </Button>
                </div>
              )}
              {st.peers.length === 0 ? (
                <p className="text-xs text-[var(--color-muted)]">
                  {tr("tn_no_peers", undefined, "No other devices on your Tailscale network yet.")}
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
              {tr("tn_wait_approval", undefined, "Waiting for this PS5 to be approved in your Tailscale admin page.")}
            </p>
          ) : (
            <p className="flex items-center gap-1 text-xs text-[var(--color-muted)]">
              <Loader2 size={12} className="animate-spin" />
              {tr("tn_connecting", undefined, "Connecting to Tailscale…")}
            </p>
          )}
          <TailnetProblems st={st} testId="tailnet-problems" />
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
