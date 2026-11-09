import { useCallback, useEffect, useState } from "react";
import { call } from "../api";
import { t } from "../strings";

/** One device in the helper's /api/mesh answer (the device list every PS5Upload app sees). */
export interface ConsoleDevice {
  id: string;
  name: string;
  nick?: string;
  kind?: string;
  ip?: string;
  online?: boolean;
  blocked?: boolean;
  /** How it was let in: "network", "tailscale", "approved", or "" (paired with a code, or older). */
  via?: string;
}

/** A device asking to connect while the PS5 asks before new devices connect. */
export interface ConsoleAsk {
  id: string;
  name: string;
  ip?: string;
  at?: number;
}

export interface ConsoleMesh {
  devices?: ConsoleDevice[];
  asks?: ConsoleAsk[];
  settings?: { require_approval?: boolean };
}

/** Re-read this often: a device asking waits for an answer. */
export const DEVICES_POLL_MS = 4000;

/** "<device> wants to use this PS5." */
export function askLine(a: ConsoleAsk): string {
  return t("cw_dev_ask", { device: a.name || a.ip || "A device" });
}

/** How a device was let in, in words. */
export function viaLine(d: ConsoleDevice): string {
  const ip = d.ip || "";
  const tailnet = /^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(ip);
  if (d.via === "approved") return t("cw_dev_via_approved");
  if (d.via === "tailscale") return t("cw_dev_via_tailscale");
  if (d.via === "network") return t("cw_dev_via_network");
  // "" from this helper: let in another way. Nothing (an older helper): the address tells.
  if (d.via === "") return t("cw_dev_via_paired");
  return t(tailnet ? "cw_dev_via_tailscale" : "cw_dev_via_network");
}

/** A change the helper refused, in words. "self_allow": the page was opened on the device that
 *  asks (LAN access on), and a device never allows itself. */
export function meshErrLine(err: string | undefined): string {
  if (err === "self_allow") return t("cw_dev_self_allow");
  if (err === "not_found") return t("cw_dev_gone");
  return t("cw_dev_err");
}

/** The devices to list: not the blocked ones (those are the app's to manage). */
export function trustedDevices(m: ConsoleMesh | null): ConsoleDevice[] {
  return (m?.devices || []).filter((d) => !d.blocked);
}

/** The asks, for Home and Settings: who wants to use this PS5 now. */
export function useConsoleMesh(): {
  mesh: ConsoleMesh | null;
  busy: boolean;
  err: string | null;
  run: (op: "allow" | "decline" | "remove", id: string) => Promise<void>;
} {
  const [mesh, setMesh] = useState<ConsoleMesh | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const read = useCallback(async () => {
    try {
      setMesh(await call<ConsoleMesh>("/api/mesh", {}));
    } catch {
      // An older helper has no /api/mesh: nothing to show.
      setMesh(null);
    }
  }, []);
  useEffect(() => {
    void read();
    const id = window.setInterval(() => {
      if (!document.hidden) void read();
    }, DEVICES_POLL_MS);
    return () => window.clearInterval(id);
  }, [read]);
  const run = async (op: "allow" | "decline" | "remove", id: string) => {
    setBusy(true);
    setErr(null);
    try {
      // The helper answers a refused change in the reply ({ok:false, err}), not with an HTTP error.
      const r = await call<{ ok?: boolean; err?: string }>("/api/mesh", { op, id });
      if (r && r.ok === false) setErr(meshErrLine(r.err));
      await read();
    } catch {
      setErr(t("cw_dev_err"));
    } finally {
      setBusy(false);
    }
  };
  return { mesh, busy, err, run };
}

/** "Pixel wants to use this PS5: Allow / Decline", one per device asking. */
export function DeviceAsks({
  asks,
  busy,
  onAnswer,
}: {
  asks: ConsoleAsk[];
  busy: boolean;
  onAnswer: (op: "allow" | "decline", id: string) => void;
}) {
  if (asks.length === 0) return null;
  return (
    <>
      {asks.map((a) => (
        <div className="cw-msg" role="status" data-dev-ask={a.id} key={a.id}>
          <p>{askLine(a)}</p>
          <button
            type="button"
            className="cw-focusable cw-button cw-msg-action"
            data-action="device-allow"
            disabled={busy}
            onClick={() => onAnswer("allow", a.id)}
          >
            <span>{t("cw_dev_allow")}</span>
          </button>
          <button
            type="button"
            className="cw-focusable cw-button cw-msg-action"
            data-action="device-decline"
            disabled={busy}
            onClick={() => onAnswer("decline", a.id)}
          >
            <span>{t("cw_dev_decline")}</span>
          </button>
        </div>
      ))}
    </>
  );
}

/** Home's copy of the asks: someone at the TV sees them without opening Settings. */
export function HomeDeviceAsks() {
  const { mesh, busy, run } = useConsoleMesh();
  return <DeviceAsks asks={mesh?.asks || []} busy={busy} onAnswer={(op, id) => void run(op, id)} />;
}

/**
 * Settings › Devices on the console page: who is asking to use this PS5 (Allow / Decline), and the
 * devices allowed to use it, with how each was let in and Remove. The same list the app's Consoles
 * page shows; renaming, blocking and the approval switch stay the app's.
 */
export function DevicesSection() {
  const { mesh, busy, err, run } = useConsoleMesh();
  const [askRemove, setAskRemove] = useState<string | null>(null);
  if (!mesh) return null;
  const list = trustedDevices(mesh);
  return (
    <section className="cw-home-section" data-setting="devices">
      <h2 className="cw-h2">{t("cw_dev_title")}</h2>
      <p className="cw-dim cw-setting-note">
        {t(mesh.settings?.require_approval ? "cw_dev_sub_ask" : "cw_dev_sub_open")}
      </p>
      <DeviceAsks asks={mesh.asks || []} busy={busy} onAnswer={(op, id) => void run(op, id)} />
      {list.length === 0 ? <p className="cw-dim cw-setting-note">{t("cw_dev_none")}</p> : null}
      {list.map((d) =>
        askRemove === d.id ? (
          <div className="cw-msg" role="alert" data-dev-remove-ask={d.id} key={d.id}>
            <p>{t("cw_dev_remove_confirm", { device: d.nick || d.name || d.ip || "" })}</p>
            <button
              type="button"
              className="cw-focusable cw-button cw-msg-action"
              data-action="device-remove-yes"
              disabled={busy}
              onClick={() => {
                setAskRemove(null);
                void run("remove", d.id);
              }}
            >
              <span>{t("cw_dev_remove")}</span>
            </button>
            <button
              type="button"
              className="cw-focusable cw-button cw-msg-action"
              data-action="device-remove-no"
              onClick={() => setAskRemove(null)}
            >
              <span>{t("cw_tn_keep")}</span>
            </button>
          </div>
        ) : (
          <div className="cw-setting" data-dev={d.id} key={d.id}>
            <button
              type="button"
              className="cw-focusable cw-button"
              data-action="device-remove"
              disabled={busy || !!d.online}
              title={d.online ? t("cw_dev_remove_online") : undefined}
              onClick={() => setAskRemove(d.id)}
            >
              <span>{d.nick || d.name || d.ip}</span>
              <span className={`cw-tag${d.online ? " is-on" : ""}`}>
                {t(d.online ? "cw_dev_online" : "cw_dev_remove")}
              </span>
            </button>
            <p className="cw-dim cw-setting-note">{viaLine(d)}</p>
          </div>
        ),
      )}
      {err ? (
        <div className="cw-msg is-error" role="alert">
          {err}
        </div>
      ) : null}
    </section>
  );
}
