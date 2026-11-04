import { useTr } from "../../state/lang";

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

/**
 * Which network a link (invite, phone controller, watch) is for: this computer's home network,
 * or its Tailscale address while the Tailscale app is connected, for someone away from home.
 * A computer on two home networks also picks which one.
 */
export function NetworkChoice({
  urls,
  tailscale,
  current,
  onPick,
  otherNetwork,
  testId,
  addressTestId,
}: {
  urls: string[];
  /** This computer's Tailscale address, from the engine, or null. */
  tailscale: string | null;
  /** The link shown now. */
  current: string | null;
  onPick: (url: string) => void;
  /** "Pick the network the friend is on" for a computer on two home networks. */
  otherNetwork: string;
  /** The block's test id; `-network`, `-home`, `-tailscale` and `-tailscale-hint` hang off it. */
  testId: string;
  /** Each home-network address button's test id. */
  addressTestId: string;
}) {
  const tr = useTr();
  const { lan, tailnet } = splitByNetwork(urls, tailscale);
  const onTailnet = !!tailnet && !!current && urlHost(current) === tailscale;
  if (!(tailnet && lan.length > 0) && !onTailnet && lan.length < 2) return null;
  return (
    <div className="space-y-2" data-testid={testId}>
      {tailnet && lan.length > 0 && (
        <div className="flex flex-wrap gap-1.5" role="group" data-testid={`${testId}-network`}>
          <button
            type="button"
            aria-pressed={!onTailnet}
            onClick={() => onPick(lan[0])}
            className="chip min-h-8 px-3 text-xs"
            data-testid={`${testId}-home`}
          >
            {tr("net_home", undefined, "This network")}
          </button>
          <button
            type="button"
            aria-pressed={onTailnet}
            onClick={() => onPick(tailnet)}
            className="chip min-h-8 px-3 text-xs"
            data-testid={`${testId}-tailscale`}
          >
            {tr("net_tailscale", undefined, "Tailscale")}
          </button>
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
      {!onTailnet && lan.length > 1 && (
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
