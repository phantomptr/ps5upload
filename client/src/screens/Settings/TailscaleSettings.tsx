import { useCallback, useEffect, useRef, useState } from "react";
import { Cable, ChevronDown, ChevronRight, ExternalLink, Loader2 } from "lucide-react";
import { useLocation, useNavigate } from "react-router";
import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { requestRosterPull } from "../../state/rosterShare";
import { DEVICES_READ_MS, DeviceList, orderDevices, peerRows } from "../Connection/TailnetDevices";
import {
  addTailnetConsole,
  appTailnetOp,
  appTailnetStatus,
  expiryDays,
  shouldAskSystemChoice,
  type AppTailnetOp,
  type AppTailnetOpts,
  type AppTailnetStatus,
} from "../../api/tailnet";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { QrCode } from "../Play/PhonePads";
import { humanizeCaught } from "../../lib/humanizeError";
import { TailnetProblems } from "../Connection/TailnetProblems";
import { NameField, OnPausedSwitch, RenewButton } from "../Connection/TailnetBits";

/**
 * Settings › Tailscale: this device on Tailscale. Either PS5Upload's own node, built into the app
 * (no Tailscale app, no VPN), or, when the official Tailscale app already runs here, that app.
 * Either way the app reaches a console that is on Tailscale ("Put this PS5 on Tailscale" on the
 * Connection screen) only when its home address does not answer.
 */
export function TailscaleSettings() {
  const tr = useTr();
  const navigate = useNavigate();
  const { hash } = useLocation();
  const { confirm, dialog } = useConfirm();
  const root = useRef<HTMLDivElement>(null);
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

  // Every few seconds while the screen is visible (sign-in, connecting and the device list move on
  // their own), and at once when it comes back to the front. Nothing while hidden.
  useEffect(() => {
    void refresh();
    const t = window.setInterval(() => {
      if (!document.hidden) void refresh();
    }, DEVICES_READ_MS);
    const now = () => {
      if (!document.hidden) void refresh();
    };
    window.addEventListener("focus", now);
    document.addEventListener("visibilitychange", now);
    return () => {
      window.clearInterval(t);
      window.removeEventListener("focus", now);
      document.removeEventListener("visibilitychange", now);
    };
  }, [refresh]);

  const profiles = useRosterStore((s) => s.profiles);
  const [refreshing, setRefreshing] = useState(false);
  const [adding, setAdding] = useState<string | null>(null);
  const [addErr, setAddErr] = useState("");
  const refreshNow = async () => {
    setRefreshing(true);
    try {
      await refresh();
    } finally {
      setRefreshing(false);
    }
  };
  /** A PS5 on the tailnet that is not on this app's list: the Tailscale add flow. */
  const addPs5 = async (ip: string, name: string) => {
    setAdding(ip);
    setAddErr("");
    try {
      await addTailnetConsole(ip, name.split(".")[0].replace(/-ps5u(-\d+)?$/, ""));
      requestRosterPull(); // the console list mirror reads the engine's now
    } catch (e) {
      setAddErr(
        tr(
          "cs_add_failed",
          { err: humanizeCaught(e) },
          "Couldn't add this PS5 ({err}). Check that it is on and that the address is right, then press Add again.",
        ),
      );
    } finally {
      setAdding(null);
    }
  };

  const run = async (op: AppTailnetOp, opts: AppTailnetOpts = {}) => {
    setBusy(true);
    setErr("");
    try {
      setSt(await appTailnetOp(op, opts));
    } catch (e) {
      setErr(tr("tn_app_failed", { err: humanizeCaught(e) }, "Could not change Tailscale on this device: {err}"));
    } finally {
      setBusy(false);
    }
  };

  const signOut = async () => {
    const ok = await confirm({
      title: tr("tn_app_sign_out_title", undefined, "Sign this device out of Tailscale?"),
      message: tr(
        "tn_app_sign_out_warn",
        undefined,
        "Its sign-in is forgotten: using Tailscale again needs a new sign-in, and this device may get a new address. To stop it for a while, switch it to Paused instead.",
      ),
      confirmLabel: tr("tn_sign_out", undefined, "Sign out"),
      destructive: true,
    });
    if (ok) await run("logout");
  };

  // The Connection screen's "Open Settings › Tailscale" lands here.
  useEffect(() => {
    if (hash === "#tailscale") root.current?.scrollIntoView({ block: "start", behavior: "smooth" });
  }, [hash]);

  const state = st?.state ?? "off";
  const days = st ? expiryDays(st.key_expiry) : null;
  const setUp = !!st && (st.enabled || !!st.signed_in);
  const usingSystem = !!st && !setUp && st.backend === "system";
  const defaultName = st?.default_hostname ?? "";
  return (
    <div ref={root} id="tailscale" className="flex flex-col gap-2 scroll-mt-4" data-testid="tailscale-settings">
      {dialog}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "tn_app_sub",
          undefined,
          "Adds this phone or computer to your Tailscale network, so the app can reach your PS5 when you're away from home. No Tailscale app needed. Do this on each device you play from.",
        )}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <p className="text-xs text-[var(--color-muted)]" data-testid="tailscale-ps5-step">
          {tr(
            "tn_app_ps5_step_v2",
            undefined,
            "Your PS5 joins separately, once, at home: open Connection and press Set up next to Tailscale on the PS5's card.",
          )}
        </p>
        <Button size="sm" variant="ghost" leftIcon={<Cable size={14} />} onClick={() => navigate("/connection")}>
          {tr("tn_open_connection", undefined, "Open Connection")}
        </Button>
      </div>
      {shouldAskSystemChoice(st) ? (
        <div
          className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] p-2"
          data-testid="tailscale-system-ask"
        >
          <p className="text-xs">
            {tr(
              "tn_system_ask",
              undefined,
              "Tailscale is already on this device. Use it (recommended) or sign PS5Upload in as its own device?",
            )}
          </p>
          <div className="flex flex-wrap gap-2">
            <Button size="sm" disabled={busy} onClick={() => void run("system_choice", { choice: "system" })} data-testid="tailscale-use-system">
              {tr("tn_system_use", undefined, "Use it (recommended)")}
            </Button>
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run("system_choice", { choice: "own" })}>
              {tr("tn_system_own", undefined, "Sign PS5Upload in as its own device")}
            </Button>
          </div>
        </div>
      ) : usingSystem ? (
        <div className="flex flex-col gap-1" data-testid="tailscale-system-in-use">
          <p className="text-xs">
            {st?.system?.ip
              ? tr("tn_system_in_use_ip", { ip: st.system.ip }, "Using the Tailscale app on this device ({ip}).")
              : tr("tn_system_in_use", undefined, "Using the Tailscale app on this device.")}
          </p>
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "tn_system_in_use_note",
              undefined,
              "When a PS5 on Tailscale does not answer at home, the app reaches it through the Tailscale app. Keep the Tailscale app connected.",
            )}
          </p>
          <div>
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run("system_choice", { choice: "own" })}>
              {tr("tn_system_switch_own", undefined, "Sign PS5Upload in as its own device instead")}
            </Button>
          </div>
        </div>
      ) : null}
      {usingSystem || shouldAskSystemChoice(st) ? null : !setUp ? (
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
                  maxLength={63}
                  placeholder={st?.hostname || defaultName}
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
            <Button
              onClick={() => void run("enable", { control_url: controlUrl.trim(), hostname: deviceName.trim() })}
              disabled={busy}
              data-testid="app-tailnet-enable"
            >
              {busy ? <Loader2 size={14} className="animate-spin" /> : null}
              {tr("tn_app_enable", undefined, "Sign in to Tailscale")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2 text-xs">
            <span className="flex-1">{tr("v5_home_tailnet_self", undefined, "Tailscale on this device")}</span>
            <OnPausedSwitch
              on={!!st?.enabled}
              connecting={!!st?.enabled && state !== "running"}
              busy={busy}
              onChange={(on) => void run(on ? "enable" : "disable")}
              testId="app-tailnet-switch"
            />
          </div>
          {!st?.enabled ? (
            <p className="text-xs text-[var(--color-muted)]" data-testid="app-tailnet-paused">
              {tr("tn_app_paused", undefined, "Paused: this device is off Tailscale and keeps its sign-in. Switch it on to use it again.")}
            </p>
          ) : state === "needs-login" && st.login_url ? (
            <div className="flex flex-col gap-2">
              <p className="text-xs">
                {tr("tn_app_sign_in", undefined, "Sign in with your Tailscale account to add this device to your Tailscale network.")}
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
              {st.renamed_to ? (
                <p className="text-xs text-[var(--color-muted)]" data-testid="app-tailnet-renamed">
                  {tr(
                    "tn_renamed_by_tailscale",
                    { name: st.renamed_to },
                    "Another device already had that name, so Tailscale named this one {name}.",
                  )}
                </p>
              ) : null}
              <p className="text-xs text-[var(--color-muted)]">
                {tr(
                  "tn_app_away_note",
                  undefined,
                  "When a PS5 on Tailscale does not answer at home, the app reaches it through Tailscale by itself.",
                )}
              </p>
              <DeviceList
                devices={orderDevices(peerRows(st.peers), {
                  self: st.ip ? { name: st.name, ip: st.ip } : null,
                  consoles: st.consoles ?? [],
                  profiles,
                })}
                refreshing={refreshing}
                onRefresh={() => void refreshNow()}
                onAdd={(d) => void addPs5(d.ip, d.name)}
                adding={adding}
              />
              {addErr ? <p className="text-xs text-[var(--color-bad)]">{addErr}</p> : null}
            </div>
          ) : state === "wait-approval" ? (
            <p className="text-xs">
              {tr("tn_app_wait_approval", undefined, "Waiting for this device to be approved in your Tailscale admin page.")}
            </p>
          ) : (
            <p className="flex items-center gap-1 text-xs text-[var(--color-muted)]">
              <Loader2 size={12} className="animate-spin" />
              {tr("tn_connecting", undefined, "Connecting to Tailscale…")}
            </p>
          )}
          <TailnetProblems st={st} testId="app-tailnet-problems" />
          {days !== null ? (
            <p className="text-xs text-[var(--color-warn)]">
              {tr(
                "tn_app_key_expires",
                { days },
                "This device's Tailscale key expires in {days} days. Renew it to stay on Tailscale.",
              )}
            </p>
          ) : null}
          <NameField
            current={st?.hostname ?? ""}
            placeholder={defaultName}
            busy={busy}
            onSave={(name) => void run("rename", { hostname: name })}
            testId="app-tailnet-name"
          />
          <div className="flex flex-wrap gap-2">
            {st?.enabled ? <RenewButton busy={busy} onRenew={() => void run("renew")} testId="app-tailnet-renew" /> : null}
            <Button size="sm" variant="ghost" onClick={() => void signOut()} disabled={busy} data-testid="app-tailnet-logout">
              {tr("tn_sign_out", undefined, "Sign out")}
            </Button>
          </div>
        </div>
      )}
      {err ? <p className="text-xs text-[var(--color-bad)]">{err}</p> : null}
    </div>
  );
}
