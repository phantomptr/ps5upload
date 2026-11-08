import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import qrcode from "qrcode-generator";
import { Check, Copy, RefreshCw, Smartphone, X } from "lucide-react";

import { Button } from "../../components";
import { writeClipboard } from "../../lib/clipboard";
import { getEngineUrl } from "../../state/engine";
import { useTr } from "../../state/lang";
import { netInviteNote, NetworkChoice, saveNetwork, savedNetwork, splitByNetwork, urlHost, type NetworkKind } from "./NetworkChoice";
import { readNetInvite, readPeers, type NetInvite, type NetPeer } from "./playTogetherState";

/** The phones link of this engine's Remote Play session (pad_link.rs). */
export interface PadLinkState {
  open: boolean;
  /** The QR code's address, while the link is open. */
  url: string | null;
  /** Every address of this computer a phone may reach, best first (`url` is the first). */
  urls: string[];
  /** This computer's Tailscale address (one of `urls`), while Tailscale is connected. */
  tailscale: string | null;
  /** What the phone app connects to (it asks; this computer allows). */
  pad_ws: string | null;
  /** Internet links may be made (Settings › Play Together); null: an engine from before them. */
  internetInvites: boolean | null;
  /** The internet link, once made (POST /api/ps5/rp/pad-link/internet). */
  internet: NetInvite | null;
  pads: Array<{ id: number; name: string; watch?: boolean; control?: boolean; via?: "internet" }>;
  /** Phones waiting for this computer's answer (an internet phone always asks first). */
  pending: Array<{ id: number; name: string; watch?: boolean; via?: "internet" }>;
  /** Internet phones' connections the engine knows of: those still connecting, or that failed. */
  netPeers: NetPeer[];
}

/** A phone's name on this computer's list: what it calls itself, in this app's language when it
 *  is one of the /pad page's device words (or gave none). */
export function phoneName(tr: ReturnType<typeof useTr>, name: string): string {
  switch (name) {
    case "iphone":
      return tr("rp_phone_name_iphone", undefined, "iPhone");
    case "ipad":
      return tr("rp_phone_name_ipad", undefined, "iPad");
    case "android":
      return tr("rp_phone_name_android", undefined, "Android phone");
    case "":
    case "phone":
      return tr("rp_phone_name_phone", undefined, "Phone");
    default:
      return name;
  }
}

const EMPTY: PadLinkState = {
  open: false,
  url: null,
  urls: [],
  tailscale: null,
  pad_ws: null,
  internetInvites: null,
  internet: null,
  pads: [],
  pending: [],
  netPeers: [],
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
          via?: unknown;
        };
        return {
          id: o.id,
          name: o.name,
          ...(o.watch === true ? { watch: true } : {}),
          ...(o.control === true ? { control: true } : {}),
          ...(o.via === "internet" ? { via: "internet" as const } : {}),
        };
      });
  return {
    open: o.open === true,
    internetInvites: typeof o.internet_invites === "boolean" ? o.internet_invites : null,
    internet: readNetInvite(o.internet),
    url: typeof o.url === "string" ? o.url : null,
    urls: Array.isArray(o.urls)
      ? o.urls.filter((u): u is string => typeof u === "string")
      : [],
    tailscale: typeof o.tailscale === "string" ? o.tailscale : null,
    pad_ws: typeof o.pad_ws === "string" ? o.pad_ws : null,
    pads: list(o.pads),
    pending: list(o.pending).map(({ id, name, watch, via }) => ({ id, name, ...(watch ? { watch } : {}), ...(via ? { via } : {}) })),
    netPeers: readPeers((o.internet as { peers?: unknown } | null | undefined)?.peers),
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

/** The internet link (made with the link, the same one again unless `renew`: a new room and key,
 *  the old link stops working). A refusal says why (`internet_off`, `hub_url`…). */
export async function padInternet(
  consoleHost: string | null,
  renew = false,
): Promise<{ state: PadLinkState | null; error: string | null }> {
  try {
    const r = await fetch(`${getEngineUrl()}/api/ps5/rp/pad-link/internet`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ console: consoleHost, new: renew }),
    });
    const v: unknown = await r.json().catch(() => null);
    if (r.ok) return { state: readPadLink(v), error: null };
    const e = (v as { error?: unknown } | null)?.error;
    return { state: null, error: typeof e === "string" ? e : `http_${r.status}` };
  } catch {
    return { state: null, error: "engine" };
  }
}

/** The watch party's link: the controller link, opened to watch (pad page, `w=1`). */
export const watchUrl = (padUrl: string) => `${padUrl}&w=1`;

/** A QR code as an SVG path: crisp at any size, no canvas. */
export function QrCode({
  text,
  label,
  size = 192,
  testId = "rp-pad-qr",
}: {
  text: string;
  label: string;
  size?: number;
  testId?: string;
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
      data-testid={testId}
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
  // The network the link is for: the same remembered choice as Play Together's invites.
  const [network, setNetwork] = useState<NetworkKind>(savedNetwork);
  // Why the engine would not make the internet link (`internet_off`, `hub_url`…), or null.
  const [netWhy, setNetWhy] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const asking = useRef(false);
  // An engine from before internet links says nothing of them: no Internet choice there.
  const netOffered = link.internetInvites !== null;
  const onInternet = netOffered && network === "internet";
  /** Asks the engine for the internet link; `renew`: a new one (the old link stops working). */
  const askNet = async (renew: boolean): Promise<void> => {
    if (asking.current) return;
    asking.current = true;
    try {
      const r = await padInternet(consoleHost, renew);
      setNetWhy(r.error);
      if (r.state) onLink(r.state);
    } finally {
      asking.current = false;
    }
  };
  const pickNetwork = (kind: NetworkKind) => {
    setNetwork(kind);
    saveNetwork(kind);
    setNetWhy(null);
  };
  useEffect(() => {
    void openPadLink(true, consoleHost).then((s) =>
      s ? onLink(s) : setFailed(true),
    );
  }, [consoleHost, onLink]);
  // The internet link when that is the choice and the engine has none (opening the panel, the
  // engine restarted): made once; a refusal stays shown until the host picks again.
  const needNet = onInternet && link.open && !link.internet && !netWhy && !failed;
  useEffect(() => {
    if (needNet) void askNet(false);
    // askNet reads the latest consoleHost/onLink; only the need starts it.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [needNet]);
  const urls = link.urls.length ? link.urls : link.url ? [link.url] : [];
  // Tailscale, when that was the last choice and this computer still has it.
  const lanPick = picked ?? (network === "tailscale" ? splitByNetwork(urls, link.tailscale).tailnet : null);
  const best =
    lanPick && urls.includes(lanPick) ? lanPick : (link.url ?? urls[0] ?? null);
  const base = onInternet ? (link.internet?.url ?? null) : best;
  const shown = base && mode === "watch" ? watchUrl(base) : base;
  const netNote = onInternet ? netInviteNote(tr, netWhy, link.internet) : null;
  // Internet phones not in yet: still connecting, or whose network could not reach this one (the
  // engine shows a failure for 30 s).
  const netConnecting = link.netPeers.filter((p) => p.path === "connecting");
  const netFailed = link.netPeers.filter((p) => p.path === "failed");
  const copy = async () => {
    if (shown && (await writeClipboard(shown))) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
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
      {failed || (!onInternet && link.open && !shown) ? (
        <p className="text-sm text-[var(--color-warn)]" data-testid="rp-pad-no-address">
          {tr(
            "rp_phone_no_address",
            undefined,
            "This computer has no address on the PS5's network to give a phone. Check that it is on the same network as the PS5.",
          )}
        </p>
      ) : shown || onInternet ? (
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
            {shown && (
              <QrCode
                text={shown}
                label={tr(
                  "rp_phone_qr",
                  undefined,
                  "QR code for the phone's camera",
                )}
              />
            )}
            <div className="min-w-0 flex-1 space-y-2 text-sm">
              <p>
                {mode === "watch" && onInternet
                  ? tr(
                      "rp_watch_net_how",
                      undefined,
                      "Anyone with this link can watch the stream from anywhere, on their phone, tablet or computer (up to four). Over the internet they only watch: they can't be handed the controller.",
                    )
                  : mode === "watch"
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
              {shown ? (
                <p
                  className="break-all text-xs text-[var(--color-muted)]"
                  data-testid="rp-pad-url"
                >
                  {shown}
                </p>
              ) : onInternet && netWhy ? (
                <Button
                  size="sm"
                  variant="ghost"
                  leftIcon={<RefreshCw size={14} />}
                  onClick={() => void askNet(false)}
                  data-testid="rp-pad-net-retry"
                >
                  {tr("join_retry", undefined, "Try again")}
                </Button>
              ) : (
                <p className="text-xs text-[var(--color-muted)]">…</p>
              )}
              {onInternet && shown && (
                // Someone far away can't scan the screen: the link goes by message.
                <div className="flex flex-wrap gap-2">
                  <Button
                    size="sm"
                    variant="secondary"
                    leftIcon={copied ? <Check size={14} /> : <Copy size={14} />}
                    onClick={() => void copy()}
                    data-testid="rp-pad-copy"
                  >
                    {copied ? tr("pt_copied", undefined, "Copied") : tr("pt_copy", undefined, "Copy link")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    leftIcon={<RefreshCw size={14} />}
                    onClick={() => void askNet(true)}
                    data-testid="rp-pad-new-link"
                  >
                    {tr("pt_new_link", undefined, "New link")}
                  </Button>
                </div>
              )}
              {netNote && (
                <p
                  className={`text-xs ${netNote.tone === "warn" ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}`}
                  data-testid={netNote.id.replace(/^pt-/, "rp-pad-")}
                >
                  {netNote.text}
                </p>
              )}
              <NetworkChoice
                urls={urls}
                tailscale={link.tailscale}
                current={onInternet ? null : best}
                onPick={(u) => {
                  setPicked(u);
                  pickNetwork(u && link.tailscale && urlHost(u) === link.tailscale ? "tailscale" : "home");
                }}
                otherNetwork={tr(
                  "rp_phone_other_network",
                  undefined,
                  "Nothing opens on the phone? This computer is on more than one network: pick the one the phone's Wi-Fi is on.",
                )}
                testId="rp-pad-addresses"
                addressTestId="rp-pad-address"
                internet={
                  netOffered
                    ? {
                        on: onInternet,
                        onPick: () => {
                          pickNetwork("internet");
                          if (!link.internet?.url) void askNet(false);
                        },
                      }
                    : null
                }
              />
              {!onInternet && (
                <p className="text-xs text-[var(--color-muted)]" data-testid="rp-pad-android-lan">
                  {tr(
                    "rp_phone_android_lan",
                    undefined,
                    "Android 17 or later and the page won't open? Allow the phone's browser Nearby devices: Settings › Apps › Chrome › Permissions.",
                  )}
                </p>
              )}
              {mode === "play" && !onInternet && (
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
      {link.pending.length > 0 && (
        // Waiting for this computer's answer (the same Allow as over the picture).
        <ul className="space-y-1.5" data-testid="rp-pad-pending">
          {link.pending.map((p) => (
            <li key={p.id} className="flex flex-wrap items-center gap-2 text-sm">
              <span className="min-w-0 flex-1 truncate">{askText(tr, p)}</span>
              <AllowPad p={p}>
                {(label, onClick, armed) => (
                  <Button size="sm" variant={armed ? "danger" : "primary"} onClick={onClick} data-testid="rp-pad-pending-allow" data-armed={armed || undefined}>
                    {label}
                  </Button>
                )}
              </AllowPad>
              <Button size="sm" variant="ghost" onClick={() => void decidePad(p.id, false)}>
                {tr("rp_phone_deny", undefined, "No")}
              </Button>
            </li>
          ))}
        </ul>
      )}
      {netConnecting.length + netFailed.length > 0 && (
        <ul className="space-y-1" data-testid="rp-pad-net-peers">
          {netConnecting.map((p) => (
            <li key={p.gid} className="flex items-center gap-2 text-xs" data-testid="rp-pad-net-peer">
              <span className="font-medium">{tr("rp_phone_net_phone", undefined, "A phone on the internet")}</span>
              <span className="text-[var(--color-muted)]">{tr("pt_net_connecting", undefined, "Connecting…")}</span>
            </li>
          ))}
          {netFailed.length > 0 && (
            <li className="flex items-center gap-2 text-xs text-[var(--color-warn)]" data-testid="rp-pad-net-failed">
              <span className="font-medium">{tr("rp_phone_net_phone", undefined, "A phone on the internet")}</span>
              <span>{tr("pt_net_peer_failed", undefined, "Couldn't connect from their network")}</span>
            </li>
          )}
        </ul>
      )}
      {link.pads.length > 0 && (
        <ul className="space-y-1" data-testid="rp-pad-list">
          {link.pads.map((p) => (
            <li key={p.id} className="flex items-center gap-2 text-sm">
              <span className="inline-block h-2 w-2 rounded-full bg-[var(--color-good)]" />
              <span className="flex-1 truncate">
                {phoneName(tr, p.name)}
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
                {p.via === "internet" && (
                  <span className="ms-2 text-xs text-[var(--color-muted)]" data-testid="rp-pad-via-internet">
                    {tr("net_internet", undefined, "Internet")}
                  </span>
                )}
              </span>
              {/* Over the internet a watcher only watches (the engine refuses it the controller). */}
              {p.watch && p.via !== "internet" && (
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

/** A phone asking to be a controller over the internet. Every phone plays on the stream's own
 *  controller (Player 1), signed in as this console's own profile: one Allow hands someone the host's
 *  profile, saves and trophies. A LAN QR phone is the host's own and asks nothing of the kind. */
export function playsAsHost(p: PadLinkState["pending"][number]): boolean {
  return p.via === "internet" && !p.watch;
}

/** What a tap on Allow does: arm it first for a phone that would play as the host, else allow. */
export function allowTap(p: PadLinkState["pending"][number], armed: boolean): "arm" | "allow" {
  return playsAsHost(p) && !armed ? "arm" : "allow";
}

/** Allow, made deliberate for a phone that would play as the host ([`playsAsHost`]): the first tap
 *  only arms it ("Yes, play as me"), the second allows. Any other phone: one tap, as before. */
export function AllowPad({
  p,
  children,
}: {
  p: PadLinkState["pending"][number];
  children: (label: string, onClick: () => void, armed: boolean) => ReactNode;
}) {
  const tr = useTr();
  const [armedId, setArmedId] = useState<number | null>(null);
  const armed = armedId === p.id;
  const onClick = () => {
    if (allowTap(p, armed) === "arm") {
      setArmedId(p.id);
      return;
    }
    void decidePad(p.id, true);
  };
  const label = armed
    ? tr("rp_phone_allow_as_me", undefined, "Yes, play as me")
    : tr("rp_phone_allow", undefined, "Allow");
  return <>{children(label, onClick, armed)}</>;
}

/** What a phone asking wants, in words: to play or to watch, and "· Internet" from afar. A phone
 *  over the internet that would play is told as what it is: control of the PS5 as the host. */
export function askText(tr: ReturnType<typeof useTr>, p: PadLinkState["pending"][number]): string {
  const name = phoneName(tr, p.name);
  if (playsAsHost(p)) {
    return tr(
      "rp_phone_ask_net_owner",
      { name },
      `${name} wants to control your PS5 as you: your profile, saves and trophies`,
    );
  }
  const what = p.watch
    ? tr("rp_watch_ask", { name }, `${name} wants to watch`)
    : tr("rp_phone_ask", { name }, `${name} wants to be a controller`);
  return p.via === "internet" ? `${what} · ${tr("net_internet", undefined, "Internet")}` : what;
}

/** A phone asking to be a controller (or, over the internet, to watch): shown over the picture,
 *  full screen included. */
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
      <span>{askText(tr, p)}</span>
      <AllowPad p={p}>
        {(label, onClick, armed) => (
          <button
            type="button"
            className={`rounded-full px-3 py-1 text-xs font-medium ${armed ? "bg-[var(--color-bad)] text-white" : "bg-white text-black"}`}
            onClick={onClick}
            data-testid="rp-pad-allow"
            data-armed={armed || undefined}
          >
            {label}
          </button>
        )}
      </AllowPad>
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
