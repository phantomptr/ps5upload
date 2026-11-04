import { useEffect, useMemo, useState, type ReactNode } from "react";
import { call } from "../api";
import { t } from "../strings";
import { encodeQr, qrPath } from "./qr";

/** /api/settings, the part read here: `addr` is the console's LAN address (newer helpers, LAN on). */
interface LanInfo {
  lan?: unknown;
  addr?: unknown;
}

/** The address a phone opens, or null when LAN access is off (or the helper is older and has no addr). */
export function phoneUrl(info: LanInfo | null, port: string): string | null {
  if (!info || info.lan !== true || typeof info.addr !== "string") return null;
  if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(info.addr) || info.addr.startsWith("127.")) return null;
  const p = /^\d{1,5}$/.test(port) ? port : "9180";
  return `http://${info.addr}:${p}/`;
}

/** The console's own browser (the page at 127.0.0.1): a phone or PC already has a keyboard. */
export function onConsoleBrowser(host: string = typeof location !== "undefined" ? location.hostname : ""): boolean {
  return host === "127.0.0.1" || host === "localhost";
}

/** Read once per page and kept briefly: every text dialog asks, the answer rarely changes. */
const KEEP_MS = 30_000;
let cached: { at: number; url: string | null } | null = null;
let inflight: Promise<string | null> | null = null;

function currentPort(): string {
  return typeof location !== "undefined" && location.port ? location.port : "9180";
}

/** The phone URL (cached for a moment), or null. Never rejects. */
export function loadPhoneUrl(now: number = Date.now()): Promise<string | null> {
  if (cached && now - cached.at < KEEP_MS) return Promise.resolve(cached.url);
  if (!inflight) {
    inflight = call<LanInfo>("/api/settings")
      .then(
        (info) => phoneUrl(info, currentPort()),
        () => null,
      )
      .then((url) => {
        cached = { at: Date.now(), url };
        inflight = null;
        return url;
      });
  }
  return inflight;
}

/** Settings changed LAN access: the next dialog asks again. */
export function forgetPhoneUrl(): void {
  cached = null;
}

/**
 * "Continue on your phone": in dialogs where the user types (rename, bulk
 * rename, go to path), when LAN access is on, a QR code and the address of this
 * page on the console's IP, so the typing can happen on a phone. Nothing shows
 * while LAN access is off (the address would not answer), nor when the page is
 * already open on a phone or PC (not at 127.0.0.1).
 */
export function PhoneHandoff({
  host,
  title,
  body,
  onOff,
  always,
}: {
  host?: string;
  title?: string;
  body?: string;
  /** Shown instead while LAN access is off (default: nothing). */
  onOff?: ReactNode;
  /** Show it whatever address the page was opened by (the caller knows it is the console's browser). */
  always?: boolean;
} = {}) {
  const here = !!always || onConsoleBrowser(host);
  const [url, setUrl] = useState<string | null>(here ? (cached?.url ?? null) : null);
  useEffect(() => {
    if (!here) return;
    let live = true;
    void loadPhoneUrl().then((u) => {
      if (live) setUrl(u);
    });
    return () => {
      live = false;
    };
  }, [here]);
  const path = useMemo(() => {
    if (!url) return null;
    try {
      const q = encodeQr(url, "M");
      return { d: qrPath(q), n: q.size + 8 };
    } catch {
      return null;
    }
  }, [url]);
  if (!here) return null;
  if (!url || !path) return onOff ?? null;
  return (
    <div className="cw-phone" data-phone="">
      <svg viewBox={`0 0 ${path.n} ${path.n}`} role="img" aria-label={t("cw_phone_qr_label")} shapeRendering="crispEdges">
        <rect width={path.n} height={path.n} fill="#fff" />
        <path d={path.d} fill="#000" />
      </svg>
      <div>
        <strong>{title ?? t("cw_phone_title")}</strong>
        <p>{body ?? t("cw_phone_body")}</p>
        <code>{url}</code>
      </div>
    </div>
  );
}
