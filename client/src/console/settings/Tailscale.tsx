import { useCallback, useEffect, useState } from "react";
import { call } from "../api";
import { t } from "../strings";
import { encodeQr, qrPath } from "../ui/qr";

/** What the helper's /api/tailnet answers (the node's status, as the app's Connection panel reads it). */
export interface ConsoleTailnet {
  enabled: boolean;
  state: string;
  login_url?: string;
  ip?: string;
  name?: string;
  error?: string;
  warning?: string;
  /** Who may reach the PS5 over Tailscale (set in the app): "listed" or "all". */
  allow_mode?: "listed" | "all";
  /** The listed devices' Tailscale addresses, comma-separated. */
  allow?: string;
  peers?: { name: string; ip: string }[];
  /** Devices the list turned away lately (newest first). */
  blocked_recent?: { ip: string; name: string }[];
}

/** "<device> tried to reach this PS5 over Tailscale" for each device the list turned away. */
export function blockedLines(s: ConsoleTailnet): { ip: string; text: string }[] {
  if (!s.enabled || s.allow_mode !== "listed") return [];
  return (s.blocked_recent || []).map((b) => ({
    ip: b.ip,
    text: t("cw_tn_blocked", { device: (b.name || "").split(".")[0] || b.ip }),
  }));
}

/** Who may reach this PS5 over Tailscale, in words (read-only here: the app sets it); null from
 *  an older helper. */
export function allowLine(s: ConsoleTailnet): string | null {
  if (!s.allow_mode) return null;
  if (s.allow_mode === "all") return t("cw_tn_allow_all");
  const ips = (s.allow || "").split(",").map((x) => x.trim()).filter(Boolean);
  const names = ips.map((ip) => (s.peers || []).find((p) => p.ip === ip)?.name.split(".")[0] || ip);
  return names.length ? t("cw_tn_allow_listed", { names: names.join(", ") }) : t("cw_tn_allow_none");
}

/** Re-read while it is on: sign-in and connecting move on their own. */
export const TAILNET_POLL_MS = 3000;

/** One line for where it stands; null when there is nothing to say beyond the switch. */
export function tailnetLine(s: ConsoleTailnet): string | null {
  // Off reads the same whether it was paused (keys kept) or signed out: the helper answers "off".
  if (!s.enabled) return t("cw_tn_off");
  if (s.state === "running" && s.ip)
    return t("cw_tn_on_as", {
      name: (s.name || "").replace(/\.$/, ""),
      ip: s.ip,
    });
  if (s.state === "needs-login" && s.login_url) return t("cw_tn_scan");
  if (s.state === "wait-approval") return t("cw_tn_wait_approval");
  return t("cw_tn_connecting");
}

/** The node's error in words. A failed DNS lookup is the one a PS5 can hit on its own: it has no
 *  resolver for Tailscale's server until the app has turned Tailscale on once. */
export function tailnetError(s: ConsoleTailnet): string | null {
  const e = (s.error || "").toLowerCase();
  if (!e) return null;
  if (e.includes("dns")) return t("cw_tn_err_dns");
  return t("cw_tn_err", { err: s.error || "" });
}

/** The sign-in link as a QR code's path, while it waits for a sign-in; null otherwise. */
export function loginQr(st: ConsoleTailnet | null): { d: string; n: number } | null {
  if (!st?.enabled || st.state !== "needs-login" || !st.login_url) return null;
  try {
    const q = encodeQr(st.login_url, "M");
    return { d: qrPath(q), n: q.size + 8 };
  } catch {
    return null;
  }
}

/**
 * Settings › Tailscale on the console page: the same switch the app's Connection panel has, for
 * someone in front of the PS5. Off keeps the keys (and the address); Sign out forgets them.
 * Sign-in is a QR code to scan with a phone.
 */
export function TailscaleSection() {
  const [st, setSt] = useState<ConsoleTailnet | null>(null);
  const [missing, setMissing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [askOut, setAskOut] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const read = useCallback(async () => {
    try {
      setSt(await call<ConsoleTailnet>("/api/tailnet", { op: "status" }));
      setMissing(false);
    } catch {
      // An older helper has no /api/tailnet: say nothing rather than an error.
      setMissing(true);
    }
  }, []);

  useEffect(() => {
    void read();
  }, [read]);
  useEffect(() => {
    if (!st?.enabled || st.state === "running") return;
    const id = window.setInterval(() => void read(), TAILNET_POLL_MS);
    return () => window.clearInterval(id);
  }, [st?.enabled, st?.state, read]);

  const run = async (op: "enable" | "disable" | "logout" | "renew" | "allow_add" | "blocked_ignore", ip?: string) => {
    setBusy(true);
    setErr(null);
    setAskOut(false);
    try {
      setSt(await call<ConsoleTailnet>("/api/tailnet", ip ? { op, ip } : { op }));
    } catch {
      setErr(t("cw_tn_err_change"));
    } finally {
      setBusy(false);
    }
  };

  const qr = loginQr(st);

  // Nothing until the helper answers: an older helper without the route shows no section at all.
  if (!st || missing) return null;
  const on = !!st?.enabled;
  const line = st ? tailnetLine(st) : null;
  const problem = st ? tailnetError(st) : null;
  return (
    <section className="cw-home-section" data-setting="tailscale">
      <h2 className="cw-h2">{t("cw_tn_title")}</h2>
      <p className="cw-dim cw-setting-note">{t("cw_tn_sub")}</p>
      {
        <>
          <div className="cw-setting">
            <button
              type="button"
              role="switch"
              aria-checked={on}
              disabled={busy}
              className="cw-focusable cw-button"
              data-action="tailscale"
              onClick={() => void run(on ? "disable" : "enable")}
            >
              <span>{t("cw_tn_switch")}</span>
              <span className={`cw-tag${on ? " is-on" : ""}`}>
                {t(on ? "cw_settings_on" : "cw_settings_off")}
              </span>
            </button>
            {line ? (
              <p className="cw-dim cw-setting-note" data-tn-line="">
                {line}
              </p>
            ) : null}
          </div>
          {qr && st.login_url ? (
            <div className="cw-phone" data-tn-login="">
              <svg
                viewBox={`0 0 ${qr.n} ${qr.n}`}
                role="img"
                aria-label={t("cw_tn_scan")}
                shapeRendering="crispEdges"
              >
                <rect width={qr.n} height={qr.n} fill="#fff" />
                <path d={qr.d} fill="#000" />
              </svg>
              <div>
                <strong>{t("cw_tn_scan_title")}</strong>
                <p>{t("cw_tn_scan_body")}</p>
                <code>{st.login_url}</code>
              </div>
            </div>
          ) : null}
          {problem ? (
            <div className="cw-msg is-error" role="alert">
              {problem}
            </div>
          ) : null}
          {on && allowLine(st) ? (
            <p className="cw-dim cw-setting-note" data-tn-allow="">
              {allowLine(st)}
            </p>
          ) : null}
          {blockedLines(st).map((b) => (
            <div className="cw-msg" role="status" data-tn-blocked={b.ip} key={b.ip}>
              <p>{b.text}</p>
              <button
                type="button"
                className="cw-focusable cw-button cw-msg-action"
                data-action="tailscale-allow"
                disabled={busy}
                onClick={() => void run("allow_add", b.ip)}
              >
                <span>{t("cw_tn_allow")}</span>
              </button>
              <button
                type="button"
                className="cw-focusable cw-button cw-msg-action"
                data-action="tailscale-ignore"
                disabled={busy}
                onClick={() => void run("blocked_ignore", b.ip)}
              >
                <span>{t("cw_tn_ignore")}</span>
              </button>
            </div>
          ))}
          {on && st.state === "running" ? (
            <div className="cw-setting">
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="tailscale-renew"
                disabled={busy}
                onClick={() => void run("renew")}
              >
                <span>{t("cw_tn_renew")}</span>
              </button>
              <p className="cw-dim cw-setting-note">{t("cw_tn_renew_note")}</p>
            </div>
          ) : null}
          {askOut ? (
            <div className="cw-msg" role="alert" data-tn-ask-out="">
              <p>{t("cw_tn_sign_out_confirm")}</p>
              <button
                type="button"
                className="cw-focusable cw-button cw-msg-action"
                data-action="tailscale-sign-out-yes"
                disabled={busy}
                onClick={() => void run("logout")}
              >
                <span>{t("cw_tn_sign_out")}</span>
              </button>
              <button
                type="button"
                className="cw-focusable cw-button cw-msg-action"
                data-action="tailscale-sign-out-no"
                onClick={() => setAskOut(false)}
              >
                <span>{t("cw_tn_keep")}</span>
              </button>
            </div>
          ) : (
            <div className="cw-setting">
              <button
                type="button"
                className="cw-focusable cw-button"
                data-action="tailscale-sign-out"
                disabled={busy}
                onClick={() => setAskOut(true)}
              >
                <span>{t("cw_tn_sign_out")}</span>
              </button>
            </div>
          )}
        </>
      }
      {err ? (
        <div className="cw-msg is-error" role="alert">
          {err}
        </div>
      ) : null}
    </section>
  );
}
