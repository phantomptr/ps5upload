import { useEffect, useMemo, useState } from "react";
import qrcode from "qrcode-generator";
import { Smartphone, X } from "lucide-react";

import { Button } from "../../components";
import { getEngineUrl } from "../../state/engine";
import { useTr } from "../../state/lang";

/** The phones link of this engine's Remote Play session (pad_link.rs). */
export interface PadLinkState {
  open: boolean;
  /** The QR code's address, while the link is open. */
  url: string | null;
  /** Every address of this computer a phone may reach, best first (`url` is the first). */
  urls: string[];
  /** What the phone app connects to (it asks; this computer allows). */
  pad_ws: string | null;
  pads: Array<{ id: number; name: string; watch?: boolean; control?: boolean }>;
  pending: Array<{ id: number; name: string }>;
}

const EMPTY: PadLinkState = {
  open: false,
  url: null,
  urls: [],
  pad_ws: null,
  pads: [],
  pending: [],
};

/** The engine's answer, checked field by field (an older or odd one never breaks the page). */
export function readPadLink(v: unknown): PadLinkState {
  const o = (v ?? {}) as Record<string, unknown>;
  const list = (x: unknown) =>
    (Array.isArray(x) ? x : [])
      .filter(
        (p): p is { id: number; name: string } =>
          !!p && typeof p.id === "number" && typeof p.name === "string",
      )
      .map((p) => {
        const o = p as {
          id: number;
          name: string;
          watch?: unknown;
          control?: unknown;
        };
        return {
          id: o.id,
          name: o.name,
          ...(o.watch === true ? { watch: true } : {}),
          ...(o.control === true ? { control: true } : {}),
        };
      });
  return {
    open: o.open === true,
    url: typeof o.url === "string" ? o.url : null,
    urls: Array.isArray(o.urls)
      ? o.urls.filter((u): u is string => typeof u === "string")
      : [],
    pad_ws: typeof o.pad_ws === "string" ? o.pad_ws : null,
    pads: list(o.pads),
    pending: list(o.pending),
  };
}

async function call(path: string, body?: object): Promise<PadLinkState | null> {
  try {
    const r = await fetch(`${getEngineUrl()}${path}`, {
      method: body ? "POST" : "GET",
      headers: body ? { "Content-Type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
    });
    return r.ok ? readPadLink(await r.json()) : null;
  } catch {
    return null;
  }
}

/** The link as the engine has it, read every second and a half while `on`. */
export function usePadLink(
  on: boolean,
  consoleHost: string | null,
): [PadLinkState, (s: PadLinkState) => void] {
  const [state, setState] = useState<PadLinkState>(EMPTY);
  useEffect(() => {
    if (!on) return;
    let alive = true;
    const q = consoleHost ? `?console=${encodeURIComponent(consoleHost)}` : "";
    const read = async () => {
      const s = await call(`/api/ps5/rp/pad-link${q}`);
      if (alive && s) setState(s);
    };
    void read();
    const t = window.setInterval(() => void read(), 1500);
    return () => {
      alive = false;
      window.clearInterval(t);
    };
  }, [on, consoleHost]);
  return [state, setState];
}

export const openPadLink = (open: boolean, consoleHost: string | null) =>
  call("/api/ps5/rp/pad-link", { open, console: consoleHost });
export const decidePad = (id: number, allow: boolean) =>
  call("/api/ps5/rp/pad-link/decide", { id, allow });
export const kickPad = (id: number) =>
  call("/api/ps5/rp/pad-link/kick", { id });
/** Hand a watcher the controller, or take it back (watch party). */
export const padControl = (id: number, on: boolean) =>
  call("/api/ps5/rp/pad-link/control", { id, on });

/** The watch party's link: the controller link, opened to watch (pad page, `w=1`). */
export const watchUrl = (padUrl: string) => `${padUrl}&w=1`;

/** A QR code as an SVG path: crisp at any size, no canvas. */
function QrCode({
  text,
  label,
  size = 192,
}: {
  text: string;
  label: string;
  size?: number;
}) {
  const { path, n } = useMemo(() => {
    const qr = qrcode(0, "M");
    qr.addData(text);
    qr.make();
    const count = qr.getModuleCount();
    let d = "";
    for (let y = 0; y < count; y++)
      for (let x = 0; x < count; x++)
        if (qr.isDark(y, x)) d += `M${x + 4} ${y + 4}h1v1h-1z`;
    return { path: d, n: count + 8 };
  }, [text]);
  return (
    <svg
      viewBox={`0 0 ${n} ${n}`}
      width={size}
      height={size}
      role="img"
      aria-label={label}
      data-testid="rp-pad-qr"
      className="rounded-md bg-white"
    >
      <path d={path} fill="#000" shapeRendering="crispEdges" />
    </svg>
  );
}

/** "Use a phone as controller": the QR code a phone camera opens, and the phones playing. */
export function PhonePadsPanel({
  consoleHost,
  link,
  onLink,
  onClose,
}: {
  consoleHost: string | null;
  link: PadLinkState;
  onLink: (s: PadLinkState) => void;
  onClose: () => void;
}) {
  const tr = useTr();
  const [failed, setFailed] = useState(false);
  // The address the QR code carries: the best one, unless another was picked (a computer on
  // two networks: the phone's Wi-Fi may not be the PS5's).
  const [picked, setPicked] = useState<string | null>(null);
  /** Which code is shown: a phone as the controller, or a device that watches (watch party). */
  const [mode, setMode] = useState<"play" | "watch">("play");
  useEffect(() => {
    void openPadLink(true, consoleHost).then((s) =>
      s ? onLink(s) : setFailed(true),
    );
  }, [consoleHost, onLink]);
  const urls = link.urls.length ? link.urls : link.url ? [link.url] : [];
  const best =
    picked && urls.includes(picked) ? picked : (link.url ?? urls[0] ?? null);
  const shown = best && mode === "watch" ? watchUrl(best) : best;
  const hostOf = (u: string) => {
    try {
      return new URL(u).hostname;
    } catch {
      return u;
    }
  };

  return (
    <div
      className="space-y-3 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-4"
      data-testid="rp-phone-pads"
    >
      <div className="flex items-center gap-2">
        <Smartphone size={16} />
        <h3 className="flex-1 text-sm font-semibold">
          {tr("rp_phone_title", undefined, "Use a phone as a controller")}
        </h3>
        <button
          type="button"
          onClick={onClose}
          aria-label={tr("rp_phone_close", undefined, "Close")}
          className="rounded p-1 hover:bg-[var(--color-surface-3)]"
        >
          <X size={16} />
        </button>
      </div>
      {failed || (link.open && !shown) ? (
        <p className="text-sm text-[var(--color-warn)]">
          {tr(
            "rp_phone_no_address",
            undefined,
            "This computer has no address on the PS5's network to give a phone. Check that it is on the same network as the PS5.",
          )}
        </p>
      ) : shown ? (
        <>
          <div className="flex gap-1.5" role="group" data-testid="rp-pad-mode">
            {(["play", "watch"] as const).map((m) => (
              <button
                key={m}
                type="button"
                aria-pressed={mode === m}
                onClick={() => setMode(m)}
                className="chip min-h-8 px-3 text-xs"
                data-testid={`rp-pad-mode-${m}`}
              >
                {m === "play"
                  ? tr("rp_phone_mode_play", undefined, "As a controller")
                  : tr("rp_phone_mode_watch", undefined, "To watch")}
              </button>
            ))}
          </div>
          <div className="flex flex-wrap items-start gap-4">
            <QrCode
              text={shown}
              label={tr(
                "rp_phone_qr",
                undefined,
                "QR code for the phone's camera",
              )}
            />
            <div className="min-w-0 flex-1 space-y-2 text-sm">
              <p>
                {mode === "watch"
                  ? tr(
                      "rp_watch_how",
                      undefined,
                      "Anyone on this network can scan this to watch the stream on their phone, tablet or computer (up to four). They only watch, until you hand one of them the controller below.",
                    )
                  : tr(
                      "rp_phone_how",
                      undefined,
                      "Scan this with the phone's camera. The phone opens a controller; turn it sideways. It plays along with this computer's controls.",
                    )}
              </p>
              {mode === "watch" && (
                <p className="text-xs text-[var(--color-muted)]">
                  {tr(
                    "rp_watch_cost",
                    undefined,
                    "Each watcher gets its own copy of the stream: on a busy network, fewer watchers keep your own picture smooth.",
                  )}
                </p>
              )}
              <p
                className="break-all text-xs text-[var(--color-muted)]"
                data-testid="rp-pad-url"
              >
                {shown}
              </p>
              {urls.length > 1 && (
                <div className="space-y-1" data-testid="rp-pad-addresses">
                  <p className="text-xs text-[var(--color-muted)]">
                    {tr(
                      "rp_phone_other_network",
                      undefined,
                      "Nothing opens on the phone? This computer is on more than one network: pick the one the phone's Wi-Fi is on.",
                    )}
                  </p>
                  <div className="flex flex-wrap gap-1.5">
                    {urls.map((u) => (
                      <button
                        key={u}
                        type="button"
                        aria-pressed={u === shown}
                        onClick={() => setPicked(u)}
                        className="chip min-h-8 px-3 font-mono text-xs"
                        data-testid="rp-pad-address"
                      >
                        {hostOf(u)}
                      </button>
                    ))}
                  </div>
                </div>
              )}
              <p className="text-xs text-[var(--color-muted)]" data-testid="rp-pad-android-lan">
                {tr(
                  "rp_phone_android_lan",
                  undefined,
                  "Android 17 or later and the page won't open? Allow the phone's browser Nearby devices: Settings › Apps › Chrome › Permissions.",
                )}
              </p>
              {mode === "play" && (
                <p className="text-xs text-[var(--color-muted)]">
                  {tr(
                    "rp_phone_app",
                    undefined,
                    "In the PS5Upload app on a phone, Remote Play shows this computer: tap Use this device as its controller, then allow it here.",
                  )}
                </p>
              )}
            </div>
          </div>
        </>
      ) : null}
      {link.pads.length > 0 && (
        <ul className="space-y-1" data-testid="rp-pad-list">
          {link.pads.map((p) => (
            <li key={p.id} className="flex items-center gap-2 text-sm">
              <span className="inline-block h-2 w-2 rounded-full bg-[var(--color-good)]" />
              <span className="flex-1 truncate">
                {p.name}
                {p.watch && (
                  <span className="ms-2 text-xs text-[var(--color-muted)]">
                    {p.control
                      ? tr(
                          "rp_watch_has_control",
                          undefined,
                          "watching, has the controller",
                        )
                      : tr("rp_watch_watching", undefined, "watching")}
                  </span>
                )}
              </span>
              {p.watch && (
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void padControl(p.id, !p.control)}
                  data-testid="rp-watch-control"
                >
                  {p.control
                    ? tr(
                        "rp_watch_take_back",
                        undefined,
                        "Take the controller back",
                      )
                    : tr("rp_watch_give", undefined, "Hand the controller")}
                </Button>
              )}
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void kickPad(p.id)}
              >
                {tr("rp_phone_kick", undefined, "Disconnect")}
              </Button>
            </li>
          ))}
        </ul>
      )}
      {link.open && (
        <Button
          size="sm"
          variant="secondary"
          onClick={() => {
            void openPadLink(false, consoleHost).then((s) => s && onLink(s));
            onClose();
          }}
          data-testid="rp-pad-link-close"
        >
          {tr("rp_phone_stop", undefined, "Close the link (every phone stops)")}
        </Button>
      )}
    </div>
  );
}

/** A phone asking to be a controller: shown over the picture, full screen included. */
export function PadAsks({ pending }: { pending: PadLinkState["pending"] }) {
  const tr = useTr();
  if (!pending.length) return null;
  const p = pending[0];
  return (
    <div
      className="pointer-events-auto absolute left-1/2 top-16 z-40 flex -translate-x-1/2 flex-wrap items-center gap-2 rounded-full bg-black/85 px-4 py-2 text-sm text-white shadow-lg"
      data-testid="rp-pad-ask"
      onClick={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <Smartphone size={14} />
      <span>
        {tr(
          "rp_phone_ask",
          { name: p.name },
          `${p.name} wants to be a controller`,
        )}
      </span>
      <button
        type="button"
        className="rounded-full bg-white px-3 py-1 text-xs font-medium text-black"
        onClick={() => void decidePad(p.id, true)}
        data-testid="rp-pad-allow"
      >
        {tr("rp_phone_allow", undefined, "Allow")}
      </button>
      <button
        type="button"
        className="rounded-full bg-white/15 px-3 py-1 text-xs"
        onClick={() => void decidePad(p.id, false)}
      >
        {tr("rp_phone_deny", undefined, "No")}
      </button>
    </div>
  );
}
