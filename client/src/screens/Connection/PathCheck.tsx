import { useEffect, useState } from "react";
import { Globe, House } from "lucide-react";
import { useTr, type Translator } from "../../state/lang";
import { deviceTailscale, tailnetCheck, type PathCheck as Check, type TailnetCheck } from "../../api/tailnet";

/** What each port is, in words. */
function portName(tr: Translator, port: number): string {
  switch (port) {
    case 9021:
      return tr("pc_port_loader", undefined, "Payload loader (9021)");
    case 9120:
      return tr("pc_port_helper", undefined, "PS5Upload helper (9120)");
    case 9295:
      return tr("pc_port_rp", undefined, "Remote Play (9295)");
    default:
      return String(port);
  }
}

/** One line for this device's Tailscale. */
export function deviceLine(tr: Translator, c: TailnetCheck | null): string {
  switch (deviceTailscale(c?.device)) {
    case "builtin-on":
      return tr("pc_device_builtin_on", undefined, "This device: PS5Upload's own Tailscale is on.");
    case "builtin-connecting":
      return tr("pc_device_builtin_connecting", undefined, "This device: PS5Upload's own Tailscale is connecting.");
    case "builtin-paused":
      return tr("pc_device_builtin_paused", undefined, "This device: PS5Upload's own Tailscale is paused.");
    case "system":
      return tr("pc_device_system", undefined, "This device: the Tailscale app is on.");
    case "signed-out":
    case "none":
      return tr("pc_device_none", undefined, "This device is not on Tailscale.");
  }
}

function Lines({ checks, tr }: { checks: Check[]; tr: Translator }) {
  return (
    <ul className="ms-5 flex flex-col gap-0.5">
      {checks.map((c) => (
        <li key={c.port} className="flex justify-between gap-2">
          <span>{portName(tr, c.port)}</span>
          <span className={c.open ? "text-[var(--color-good)]" : "text-[var(--color-muted)]"}>
            {c.open ? tr("pc_answered", undefined, "answers") : tr("pc_no_answer", undefined, "no answer")}
          </span>
        </li>
      ))}
    </ul>
  );
}

/**
 * The Connection check by path: what answered on the home network, and, when nothing did and
 * this PS5 is on Tailscale, what answered over Tailscale. Plus this device's own Tailscale state.
 * Runs once per check (`nonce`); nothing in the background.
 */
export default function PathCheck({ host, nonce }: { host: string; nonce: number }) {
  const tr = useTr();
  const [c, setC] = useState<TailnetCheck | null>(null);
  useEffect(() => {
    if (!nonce || !host) return;
    let live = true;
    setC(null);
    void tailnetCheck(host).then((r) => {
      if (live) setC(r);
    });
    return () => {
      live = false;
    };
  }, [host, nonce]);
  if (!c) return null;
  const via =
    c.tailnet?.backend === "system"
      ? tr("pc_via_system", { ip: c.tailnet.ip }, "Over Tailscale, through the Tailscale app ({ip})")
      : c.tailnet
        ? tr("pc_via_builtin", { ip: c.tailnet.ip }, "Over Tailscale, through PS5Upload's own ({ip})")
        : "";
  return (
    <div className="mt-3 flex flex-col gap-1 text-xs" data-testid="path-check">
      <div className="flex items-center gap-1 font-medium">
        <House size={12} aria-hidden />
        {tr("pc_home", undefined, "On the home network")}
      </div>
      <Lines checks={c.home} tr={tr} />
      {c.tailnet ? (
        <>
          <div className="mt-1 flex items-center gap-1 font-medium" data-testid="path-check-tailnet">
            <Globe size={12} aria-hidden />
            {via}
          </div>
          <Lines checks={c.tailnet.checks} tr={tr} />
          {c.used !== "none" ? (
            <p className="text-[var(--color-good)]">
              {tr("pc_used_tailnet", undefined, "The home network did not answer, so the app reaches this PS5 through Tailscale.")}
            </p>
          ) : null}
        </>
      ) : c.used === "none" && c.tailnet_ip ? (
        <p className="text-[var(--color-muted)]">
          {tr(
            "pc_tailnet_not_tried",
            undefined,
            "This PS5 is on Tailscale, but this device is not, so Tailscale was not tried. Sign in under Settings › Tailscale.",
          )}
        </p>
      ) : null}
      <p className="text-[var(--color-muted)]" data-testid="path-check-device">
        {deviceLine(tr, c)}
      </p>
    </div>
  );
}
