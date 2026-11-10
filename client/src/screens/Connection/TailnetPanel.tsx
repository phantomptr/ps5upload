import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, CheckCircle2, ChevronDown, ChevronRight, Circle, ExternalLink, Globe, Loader2, Settings2 } from "lucide-react";
import { useNavigate } from "react-router";
import { Button } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { useTr } from "../../state/lang";
import {
  allowList,
  appTailnetOp,
  appTailnetStatus,
  expiryDays,
  tailnetOp,
  tailnetStatus,
  tailnetVia,
  toggleAllowed,
  TailnetUnsupportedError,
  type AppTailnetStatus,
  type TailnetConsoleOp,
  type TailnetEnable,
  type TailnetBlocked,
  type TailnetStatus,
} from "../../api/tailnet";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { consoleAddr } from "../../lib/addr";
import { QrCode } from "../Play/PhonePads";
import { humanizeCaught } from "../../lib/humanizeError";
import { TailnetProblems } from "./TailnetProblems";
import { KeyExpiryHint, NameField, OnPausedSwitch, RenewButton } from "./TailnetBits";
import { DeviceRow, orderDevices, peerRows } from "./TailnetDevices";
import { useRosterStore } from "../../state/roster";

/** "Test it now" sends the PS5 through Tailscale from home for this long. */
const TEST_SECS = 120;

/**
 * "Put this PS5 on Tailscale": the console joins the user's tailnet through the helper and
 * serves only Remote Play and PS5Upload to it, and only to the devices on its list. Sign-in is
 * Tailscale's own page (a link and a QR code for the phone); the advanced fields are for a
 * Headscale server. A guided setup: this PS5 (1), each device you play from (2), then "Test it
 * now" (3), which sends this PS5 through Tailscale from home so you can see it work.
 */
export default function TailnetPanel({
  host,
  startTest = 0,
}: {
  host: string;
  /** Bumped by the console card's Test: the panel starts "Try Tailscale now" itself, so the test,
   *  its end and "Back to the home network" live in one place. */
  startTest?: number;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const [st, setSt] = useState<TailnetStatus | null>(null);
  /** This device's own Tailscale (Settings › Tailscale): step 2. null until read. */
  const [me, setMe] = useState<AppTailnetStatus | null>(null);
  const [unsupported, setUnsupported] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [controlUrl, setControlUrl] = useState("");
  const [deviceName, setDeviceName] = useState("");
  /** "Test it now": until when, and what it found. */
  const [testUntil, setTestUntil] = useState(0);
  const [testVia, setTestVia] = useState("");

  const refresh = useCallback(async () => {
    try {
      setSt(await tailnetStatus(host));
      setUnsupported(false);
    } catch (e) {
      if (e instanceof TailnetUnsupportedError) setUnsupported(true);
    }
    try {
      setMe(await appTailnetStatus());
    } catch {
      /* an engine without Tailscale: step 2 is just not shown as done */
    }
  }, [host]);

  useEffect(() => {
    void refresh();
    // While it is on, follow sign-in and paths; off, one read is enough.
    const t = window.setInterval(() => {
      if ((st?.enabled || testUntil) && !document.hidden) void refresh();
    }, 3000);
    return () => window.clearInterval(t);
  }, [refresh, st?.enabled, testUntil]);

  // While a test runs: how the engine reaches the PS5 now, and the end of the test.
  useEffect(() => {
    if (!testUntil) return;
    const t = window.setInterval(() => {
      if (Date.now() >= testUntil) {
        setTestUntil(0);
        setTestVia("");
        return;
      }
      void tailnetVia(host)
        .then((v) => setTestVia(v.via))
        .catch(() => undefined); // the next tick asks again
    }, 2000);
    return () => window.clearInterval(t);
  }, [testUntil, host]);

  const run = async (op: TailnetConsoleOp, opts: TailnetEnable = {}) => {
    setBusy(true);
    setErr("");
    try {
      setSt(await tailnetOp(host, op, opts));
    } catch (e) {
      if (e instanceof TailnetUnsupportedError) setUnsupported(true);
      else setErr(tr("tn_failed", { err: humanizeCaught(e) }, "Could not change Tailscale on this PS5: {err}"));
    } finally {
      setBusy(false);
    }
  };

  const signOut = async () => {
    const ok = await confirm({
      title: tr("tn_sign_out_title", undefined, "Sign this PS5 out of Tailscale?"),
      message: tr(
        "tn_sign_out_warn_ps5",
        undefined,
        "Its sign-in is forgotten: putting it back on Tailscale needs a new sign-in, and it may get a new address. To stop it for a while, switch it to Paused instead.",
      ),
      confirmLabel: tr("tn_sign_out", undefined, "Sign out"),
      destructive: true,
    });
    if (ok) await run("logout");
  };

  const testNow = async (on: boolean) => {
    setBusy(true);
    setErr("");
    try {
      setMe(await appTailnetOp("test_path", { host, secs: on ? TEST_SECS : 0 }));
      setTestUntil(on ? Date.now() + TEST_SECS * 1000 : 0);
      setTestVia("");
    } catch (e) {
      setErr(tr("tn_test_failed", { err: humanizeCaught(e) }, "Could not try Tailscale now: {err}"));
    } finally {
      setBusy(false);
    }
  };

  // The card's Test: once per press.
  const testNowRef = useRef(testNow);
  useEffect(() => {
    testNowRef.current = testNow;
  });
  useEffect(() => {
    if (startTest > 0) void testNowRef.current(true);
  }, [startTest]);

  const state = st?.state ?? "off";
  const psOn = !!st?.enabled && state === "running" && !!st.ip;
  const selfOn = !!me && (me.backend === "builtin" ? me.state === "running" : me.backend === "system");
  const setUp = !!st && (st.enabled || !!st.signed_in);
  const days = st ? expiryDays(st.key_expiry) : null;
  const listed = allowList(st);
  // Step 3 stays done once "Try Tailscale now" reached this PS5 (the engine keeps it per console).
  const testedAt = me?.consoles?.find((c) => consoleAddr(c.host) === consoleAddr(host))?.tested_at ?? 0;
  return (
    <section
      className="flex flex-col gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface)] p-3"
      data-testid="tailnet-panel"
    >
      {dialog}
      <div className="flex items-center gap-2 text-sm font-semibold">
        <Globe size={16} />
        <span className="flex-1">{tr("tn_title_ps5", undefined, "Tailscale for this PS5")}</span>
        {setUp && !unsupported ? (
          <OnPausedSwitch
            on={!!st?.enabled}
            connecting={!!st?.enabled && state !== "running"}
            busy={busy}
            onChange={(on) => void run(on ? "enable" : "disable")}
            testId="tailnet-switch"
          />
        ) : null}
      </div>
      <p className="text-xs text-[var(--color-muted)]">
        {tr("tn_sub_setup", undefined, "Play this PS5 when you're away from home. Set it up once, at home:")}
      </p>
      <ol className="flex flex-col gap-1 text-xs" data-testid="tailnet-steps">
        <Step done={psOn} n={1} text={tr("tn_step_ps5", undefined, "Here, at home: put this PS5 on your Tailscale network.")} />
        <Step
          done={selfOn}
          n={2}
          text={tr(
            "tn_step_devices",
            undefined,
            "On each phone or computer you'll play from: Settings › Tailscale › Sign in to Tailscale.",
          )}
        />
        <Step
          done={(testVia !== "" && testVia !== "lan") || testedAt > 0}
          n={3}
          text={tr("tn_step_test", undefined, "Test it now: reach this PS5 through Tailscale from home, to see it work.")}
        />
      </ol>
      <p className="text-xs text-[var(--color-muted)]">
        {tr("tn_only_rp", undefined, "Only Remote Play and PS5Upload can reach the PS5 this way.")}{" "}
        {tr("tn_no_wake", undefined, "A PS5 in rest mode is off Tailscale, so it can only be woken at home.")}
      </p>
      {unsupported ? (
        <p className="text-xs text-[var(--color-warn)]">
          {tr("tn_old_helper", undefined, "This PS5's helper is too old for Tailscale. Send the helper again.")}
        </p>
      ) : !setUp ? (
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
                  maxLength={63}
                  placeholder={st?.hostname || tr("tn_default_name_ps5", undefined, "This PS5's own name + -ps5u")}
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
              data-testid="tailnet-enable"
            >
              {busy ? <Loader2 size={14} className="animate-spin" /> : null}
              {tr("tn_enable", undefined, "Put this PS5 on Tailscale")}
            </Button>
          </div>
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          {!st?.enabled ? (
            <p className="text-xs text-[var(--color-muted)]" data-testid="tailnet-paused">
              {tr("tn_paused_ps5", undefined, "Paused: this PS5 is off Tailscale and keeps its sign-in. Switch it on to use it again.")}
            </p>
          ) : state === "needs-login" && st.login_url ? (
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
              {selfOn ? (
                <div className="flex flex-wrap items-center gap-2" data-testid="tailnet-test">
                  {testUntil ? (
                    <>
                      <span className="text-xs">
                        {testVia && testVia !== "lan"
                          ? tr("tn_test_working", undefined, "Working: the app reaches this PS5 through Tailscale now.")
                          : tr("tn_test_trying", undefined, "Switching to Tailscale…")}
                      </span>
                      <Button size="sm" variant="secondary" disabled={busy} onClick={() => void testNow(false)}>
                        {tr("tn_test_stop", undefined, "Back to the home network")}
                      </Button>
                    </>
                  ) : (
                    <Button size="sm" variant="secondary" disabled={busy} onClick={() => void testNow(true)} data-testid="tailnet-try-now">
                      {tr("tn_try_now", undefined, "Try Tailscale now")}
                    </Button>
                  )}
                </div>
              ) : null}
              <BlockedNotice
                blocked={st.blocked_recent ?? []}
                busy={busy}
                onAllow={(ip) => void run("allow_add", { ip })}
                onIgnore={(ip) => void run("blocked_ignore", { ip })}
              />
              <AllowSection st={st} busy={busy} listed={listed} onSave={(mode, list) => void run("allow", { allow_mode: mode, allow: list.join(",") })} />
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
          {days !== null ? (
            <p className="text-xs text-[var(--color-warn)]" data-testid="tailnet-expiry">
              {tr("tn_key_expires", { days }, "This PS5's Tailscale key expires in {days} days. Renew it to keep this PS5 on Tailscale.")}
            </p>
          ) : null}
          {st?.enabled && (days !== null || state === "running" || state === "signed-out") ? <KeyExpiryHint /> : null}
          <NameField
            current={st?.hostname ?? ""}
            placeholder={tr("tn_default_name_ps5", undefined, "This PS5's own name + -ps5u")}
            busy={busy}
            onSave={(name) => void run("rename", { hostname: name })}
            testId="tailnet-name"
          />
          <div className="flex flex-wrap gap-2">
            {st?.enabled ? <RenewButton busy={busy} onRenew={() => void run("renew")} testId="tailnet-renew" /> : null}
            <Button size="sm" variant="ghost" onClick={() => void signOut()} disabled={busy} data-testid="tailnet-logout">
              {tr("tn_sign_out", undefined, "Sign out")}
            </Button>
          </div>
        </div>
      )}
      {err ? <p className="text-xs text-[var(--color-bad)]">{err}</p> : null}
    </section>
  );
}

/** "<device> tried to reach this PS5 over Tailscale: Allow / Ignore", for each device the PS5's
 *  list turned away lately (the helper's `blocked_recent`). Allow adds it to the list. */
export function BlockedNotice({
  blocked,
  busy,
  onAllow,
  onIgnore,
}: {
  blocked: TailnetBlocked[];
  busy: boolean;
  onAllow: (ip: string) => void;
  onIgnore: (ip: string) => void;
}) {
  const tr = useTr();
  if (blocked.length === 0) return null;
  return (
    <ul className="flex flex-col gap-1 text-xs" data-testid="tailnet-blocked">
      {blocked.map((b) => (
        <li
          key={b.ip}
          className="flex flex-wrap items-center gap-2 rounded-[var(--radius-field)] border border-[var(--color-warn)] px-2 py-1"
        >
          <span className="min-w-0 flex-1 basis-[12rem]">
            {tr(
              "tn_blocked_tried",
              { device: b.name.split(".")[0] || b.ip },
              "{device} tried to reach this PS5 over Tailscale.",
            )}
          </span>
          <Button size="sm" variant="secondary" disabled={busy} onClick={() => onAllow(b.ip)} data-testid="tailnet-blocked-allow">
            {tr("tn_blocked_allow", undefined, "Allow")}
          </Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={() => onIgnore(b.ip)} data-testid="tailnet-blocked-ignore">
            {tr("tn_blocked_ignore", undefined, "Ignore")}
          </Button>
        </li>
      ))}
    </ul>
  );
}

function Step({ done, n, text }: { done: boolean; n: number; text: string }) {
  return (
    <li className="flex items-start gap-1.5">
      {done ? (
        <CheckCircle2 size={14} className="mt-px shrink-0 text-[var(--color-good)]" />
      ) : (
        <Circle size={14} className="mt-px shrink-0 text-[var(--color-muted)]" />
      )}
      <span>
        <span className="font-semibold">{n}.</span> {text}
      </span>
    </li>
  );
}

/** Who may reach this PS5 over Tailscale: the devices ticked here (the default: the ones added
 *  from PS5Upload), or every device on the Tailscale network (an opt-in, with a warning for a
 *  shared tailnet). Nothing from an older helper, which has no list. */
function AllowSection({
  st,
  busy,
  listed,
  onSave,
}: {
  st: TailnetStatus;
  busy: boolean;
  listed: string[];
  onSave: (mode: "listed" | "all", list: string[]) => void;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  if (!st.allow_mode) return null;
  const all = st.allow_mode === "all";
  const peers = st.peers.filter((p) => p.ip);
  // Listed devices that are not in the PS5's peer list now still show, by address.
  const extra = listed.filter((ip) => !peers.some((p) => p.ip === ip));
  return (
    <div className="mt-1 flex flex-col gap-1 border-t border-[var(--glass-edge)] pt-2 text-xs" data-testid="tailnet-allow">
      <span className="font-semibold">{tr("tn_allow_title", undefined, "Who can reach this PS5 over Tailscale")}</span>
      <label className="flex items-center gap-1.5">
        <input type="radio" checked={!all} disabled={busy} onChange={() => onSave("listed", listed)} />
        {tr("tn_allow_listed", undefined, "Only the devices ticked below (recommended)")}
      </label>
      <label className="flex items-center gap-1.5">
        <input type="radio" checked={all} disabled={busy} onChange={() => onSave("all", listed)} />
        {tr("tn_allow_all", undefined, "Every device on my Tailscale network")}
      </label>
      {all ? (
        <p className="flex items-start gap-1 text-[var(--color-warn)]" data-testid="tailnet-allow-all-warn">
          <AlertTriangle size={12} className="mt-px shrink-0" />
          {tr(
            "tn_allow_all_warn",
            undefined,
            "If you share your Tailscale network with other people, each of their devices could then reach Remote Play and PS5Upload on this PS5.",
          )}
        </p>
      ) : listed.length === 0 ? (
        <p className="text-[var(--color-warn)]" data-testid="tailnet-allow-empty">
          {tr(
            "tn_allow_empty",
            undefined,
            "Only devices you add can reach this PS5 over Tailscale. Open PS5Upload at home on each device once, or tick devices below.",
          )}
        </p>
      ) : null}
      {all ? null : (
        <ul className="flex flex-col gap-0.5" data-testid="tailnet-allow-list">
          {orderDevices(peerRows(peers), { profiles }).map((d) => (
            <DeviceRow
              key={d.ip}
              d={d}
              lead={
                <input
                  type="checkbox"
                  aria-label={d.name.split(".")[0] || d.ip}
                  checked={listed.includes(d.ip)}
                  disabled={busy}
                  onChange={(e) => onSave("listed", toggleAllowed(listed, d.ip, e.target.checked))}
                />
              }
            />
          ))}
          {extra.map((ip) => (
            <li key={ip}>
              <label className="flex items-center gap-1.5">
                <input type="checkbox" checked disabled={busy} onChange={() => onSave("listed", toggleAllowed(listed, ip, false))} />
                <span className="text-[var(--color-muted)]">{ip}</span>
              </label>
            </li>
          ))}
          {peers.length === 0 && extra.length === 0 ? (
            <li className="text-[var(--color-muted)]">{tr("tn_no_peers", undefined, "No other devices on your Tailscale network yet.")}</li>
          ) : null}
        </ul>
      )}
      <p className="text-[var(--color-muted)]">
        {tr(
          "tn_allow_auto",
          undefined,
          "A device that signs in to Tailscale in PS5Upload is added by itself the next time it talks to this PS5 at home.",
        )}
      </p>
    </div>
  );
}
