import { useNavigate } from "react-router";

import { safeGetItem, safeSetItem } from "../../lib/safeStorage";
import { useTr } from "../../state/lang";
import type { NetInvite } from "./playTogetherState";

type Tr = ReturnType<typeof useTr>;

/** What an internet link (a Play Together invite, or the phone link's) needs the host to know, or
 *  null: why the engine would not make one (`why`, from the call), or its link to the invite
 *  service (the hub) as the state says. */
export function netInviteNote(
  tr: Tr,
  why: string | null,
  net: NetInvite | null,
): { text: string; tone: "warn" | "muted"; id: string } | null {
  if (why === "internet_off")
    return {
      id: "pt-net-off",
      tone: "warn",
      text: tr(
        "pt_wifi_only_setting",
        undefined,
        "Invites work only on your Wi-Fi. For friends somewhere else, set Invites work to Anywhere in Settings › App › Play Together.",
      ),
    };
  if (why === "hub_url")
    return {
      id: "pt-net-hub-url",
      tone: "warn",
      text: tr("pt_net_hub_url", undefined, "The invite service address in Settings › Play Together isn't a wss:// address."),
    };
  if (why)
    return {
      id: "pt-net-failed",
      tone: "warn",
      text: `${tr("pt_net_link_failed", undefined, "An internet link could not be made. Try again.")} (${why})`,
    };
  if (!net) return null;
  if ((net.hubState === "error" || net.hubState === "reconnecting") && net.hubError)
    return {
      id: "pt-net-hub-down",
      tone: "warn",
      text: `${tr(
        "pt_net_hub_down",
        undefined,
        "Couldn't reach the PS5Upload invite service, so friends can't join with this link yet. Check this computer's internet connection.",
      )} (${net.hubError})`,
    };
  if (net.hubState === "connecting")
    return { id: "pt-net-hub-connecting", tone: "muted", text: tr("pt_net_hub_connecting", undefined, "Connecting to the invite service…") };
  return null;
}

/** Which network an invite was last made for, on this device: Play Together opens on it again. */
export type NetworkKind = "home" | "tailscale" | "internet";
const NETWORK_KEY = "ps5upload.pt_network";

export function savedNetwork(): NetworkKind {
  const v = safeGetItem(NETWORK_KEY);
  return v === "tailscale" || v === "internet" ? v : "home";
}

export function saveNetwork(kind: NetworkKind): void {
  safeSetItem(NETWORK_KEY, kind);
}

/** The host of a link (`http://10.0.0.5:19113/join#…` → `10.0.0.5`), or the text itself. */
export function urlHost(u: string): string {
  try {
    return new URL(u.includes("://") ? u : `http://${u}`).hostname;
  } catch {
    return u;
  }
}

/** The links split by network: the Tailscale one (when the engine named its address) and the
 *  home network's, best first. */
export function splitByNetwork(urls: string[], tailscale: string | null): { lan: string[]; tailnet: string | null } {
  const tailnet = tailscale ? (urls.find((u) => urlHost(u) === tailscale) ?? null) : null;
  return { lan: urls.filter((u) => u !== tailnet), tailnet };
}

/** Under the Internet choice: what it does, and one quiet line asking for support (the hub is a
 *  server PS5Upload pays for). No amounts; the link opens Thanks & Support. */
export function InternetHint({ testId }: { testId: string }) {
  const tr = useTr();
  const navigate = useNavigate();
  return (
    <div className="space-y-1">
      <p className="text-xs text-[var(--color-muted)]" data-testid={`${testId}-internet-hint`}>
        {tr(
          "net_internet_hint",
          undefined,
          "Works from anywhere: your friend's browser connects straight to this computer. Some strict networks can't connect.",
        )}
      </p>
      <p className="text-[11px] text-[var(--color-muted)] opacity-80" data-testid={`${testId}-internet-support`}>
        {tr(
          "net_internet_support",
          undefined,
          "Internet play runs on a small server we pay for. If it helps you, a donation keeps it going. Thank you!",
        )}{" "}
        <button
          type="button"
          onClick={() => navigate("/thanks")}
          className="underline underline-offset-2 hover:text-[var(--color-text)]"
          data-testid={`${testId}-internet-donate`}
        >
          {tr("thanks_support_title", undefined, "Support PS5Upload")}
        </button>
      </p>
    </div>
  );
}

/**
 * Which network a link (invite, phone controller, watch) is for: this computer's home network,
 * or its Tailscale address while the Tailscale app is connected, for someone away from home.
 * A computer on two home networks also picks which one. Play Together and the phone link also
 * offer the internet (`internet`): a link through the signalling hub for anyone anywhere.
 */
export function NetworkChoice({
  urls,
  tailscale,
  current,
  onPick,
  otherNetwork,
  testId,
  addressTestId,
  internet,
}: {
  urls: string[];
  /** This computer's Tailscale address, from the engine, or null. */
  tailscale: string | null;
  /** The link shown now. */
  current: string | null;
  /** A home-network or Tailscale link picked (null: the home network, no address known yet). */
  onPick: (url: string | null) => void;
  /** "Pick the network the friend is on" for a computer on two home networks. */
  otherNetwork: string;
  /** The block's test id; `-network`, `-home`, `-tailscale`, `-internet` and their hints hang
   *  off it. */
  testId: string;
  /** Each home-network address button's test id. */
  addressTestId: string;
  /** Offer "Internet": `on` while the internet link is the one shown. */
  internet?: { on: boolean; onPick: () => void } | null;
}) {
  const tr = useTr();
  const { lan, tailnet } = splitByNetwork(urls, tailscale);
  const onInternet = !!internet?.on;
  const onTailnet = !onInternet && !!tailnet && !!current && urlHost(current) === tailscale;
  if (!internet && !(tailnet && lan.length > 0) && !onTailnet && lan.length < 2) return null;
  return (
    <div className="space-y-2" data-testid={testId}>
      {((tailnet && lan.length > 0) || internet) && (
        <div className="flex flex-wrap gap-1.5" role="group" data-testid={`${testId}-network`}>
          <button
            type="button"
            aria-pressed={!onTailnet && !onInternet}
            onClick={() => onPick(lan[0] ?? null)}
            className="chip min-h-8 px-3 text-xs"
            data-testid={`${testId}-home`}
          >
            {tr("net_home", undefined, "This network")}
          </button>
          {tailnet && lan.length > 0 && (
            <button
              type="button"
              aria-pressed={onTailnet}
              onClick={() => onPick(tailnet)}
              className="chip min-h-8 px-3 text-xs"
              data-testid={`${testId}-tailscale`}
            >
              {tr("net_tailscale", undefined, "Tailscale")}
            </button>
          )}
          {internet && (
            <button
              type="button"
              aria-pressed={onInternet}
              onClick={internet.onPick}
              className="chip min-h-8 px-3 text-xs"
              data-testid={`${testId}-internet`}
            >
              {tr("net_internet", undefined, "Internet")}
            </button>
          )}
        </div>
      )}
      {onTailnet && (
        <p className="text-xs text-[var(--color-muted)]" data-testid={`${testId}-tailscale-hint`}>
          {tr(
            "net_tailscale_hint",
            undefined,
            "For someone away from home: their device needs Tailscale, signed in to your tailnet or with this computer shared with them (Share… in Tailscale's admin console).",
          )}
        </p>
      )}
      {onInternet && <InternetHint testId={testId} />}
      {!onTailnet && !onInternet && lan.length > 1 && (
        <div className="space-y-1">
          <p className="text-xs text-[var(--color-muted)]">{otherNetwork}</p>
          <div className="flex flex-wrap gap-1.5">
            {lan.map((u) => (
              <button
                key={u}
                type="button"
                aria-pressed={!!current && urlHost(u) === urlHost(current)}
                onClick={() => onPick(u)}
                className="chip min-h-8 px-3 font-mono text-xs"
                data-testid={addressTestId}
              >
                {urlHost(u)}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
