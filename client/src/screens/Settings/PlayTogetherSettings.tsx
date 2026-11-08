import { useEffect, useState } from "react";

import { Button, Input, Textarea, Toggle } from "../../components";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import {
  hubUrlOk,
  joinApi,
  MAX_STUN,
  parseStun,
  type NetSettings,
  type NetSettingsBody,
} from "../Play/playTogetherState";
import { PlayTogetherGuestProfiles } from "./PlayTogetherGuestProfiles";

/**
 * Settings › Play Together: which PS5 profile each guest plays as on the connected console
 * (PlayTogetherGuestProfiles), and internet invites (a link through the signalling hub, the
 * friend's browser connecting straight to this computer over WebRTC). The engine keeps one set of
 * the internet settings for every console, so any console's address reads and writes it.
 */
export function PlayTogetherSettings() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host) ?? "";
  const [net, setNet] = useState<NetSettings | null>(null);
  const [unread, setUnread] = useState(false);
  const [hub, setHub] = useState("");
  const [stun, setStun] = useState("");
  const [saveWhy, setSaveWhy] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    let alive = true;
    void joinApi.read(host).then((s) => {
      if (!alive) return;
      if (!s) {
        setUnread(true);
        return;
      }
      setNet(s.settings);
      setHub(s.settings.hubUrl);
      setStun(s.settings.stun.join("\n"));
    });
    return () => {
      alive = false;
    };
  }, [host]);

  const save = async (body: NetSettingsBody) => {
    setSaved(false);
    const r = await joinApi.netSettings(host, body);
    setSaveWhy(r.error ?? null);
    if (r.state) {
      setNet(r.state.settings);
      if (body.hub_url != null || body.stun != null) setSaved(true);
    }
  };

  if (unread) {
    return (
      <p
        className="text-xs text-[var(--color-muted)]"
        data-testid="pt-set-unread"
      >
        {tr(
          "pt_unavailable",
          undefined,
          "Play Together could not start. Check that the engine is up to date.",
        )}
      </p>
    );
  }
  if (!net) return <p className="text-xs text-[var(--color-muted)]">…</p>;
  if (net.internetInvites === null) {
    return (
      <p
        className="text-xs text-[var(--color-muted)]"
        data-testid="pt-set-old-engine"
      >
        {tr(
          "pt_set_old_engine",
          undefined,
          "Internet invites need a newer PS5Upload engine.",
        )}
      </p>
    );
  }

  const hubBad = hub.trim() !== "" && !hubUrlOk(hub);
  const stunList = parseStun(stun);
  const advancedChanged =
    hub.trim() !== net.hubUrl ||
    (stunList ?? []).join("\n") !== net.stun.join("\n");

  return (
    <div className="space-y-3" data-testid="pt-settings">
      <PlayTogetherGuestProfiles host={host} />
      <div data-testid="pt-set-internet">
        <Toggle
          checked={net.internetInvites}
          onChange={(on) => {
            setNet({ ...net, internetInvites: on });
            void save({ internet_invites: on });
          }}
          label={tr("pt_set_internet", undefined, "Allow internet invites")}
          hint={tr(
            "pt_set_internet_hint",
            undefined,
            "Play Together can make a link for friends anywhere, no Tailscale needed. Off: invites work on your home network only.",
          )}
        />
      </div>
      <div data-testid="pt-set-upnp">
        <Toggle
          checked={net.upnp}
          onChange={(on) => {
            setNet({ ...net, upnp: on });
            void save({ upnp: on });
          }}
          label={tr(
            "pt_set_upnp",
            undefined,
            "Open a port on my router automatically (UPnP)",
          )}
          hint={tr(
            "pt_set_upnp_hint",
            undefined,
            "While an internet invite is open, your router is asked for a port so more friends' networks can connect. It closes when the invite ends.",
          )}
        />
      </div>
      <details className="text-sm" data-testid="pt-set-advanced">
        <summary className="cursor-pointer select-none text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]">
          {tr("pt_set_advanced", undefined, "Advanced")}
        </summary>
        <div className="mt-3 space-y-3">
          <Input
            label={tr("pt_set_hub", undefined, "Hub address")}
            value={hub}
            onChange={(e) => {
              setHub(e.target.value);
              setSaved(false);
            }}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            hint={tr(
              "pt_set_hub_hint",
              undefined,
              "The service that introduces your friend's browser to this computer. The game itself never goes through it.",
            )}
            error={
              hubBad || saveWhy === "hub_url"
                ? tr(
                    "pt_set_hub_bad",
                    undefined,
                    "Use a wss:// address (ws:// only for this computer).",
                  )
                : undefined
            }
            data-testid="pt-set-hub"
          />
          <Textarea
            label={tr("pt_set_stun", undefined, "STUN servers")}
            value={stun}
            onChange={(e) => {
              setStun(e.target.value);
              setSaved(false);
            }}
            rows={3}
            spellCheck={false}
            hint={tr(
              "pt_set_stun_hint",
              undefined,
              "Used by both sides to find their public address. Defaults: Cloudflare and Google.",
            )}
            error={
              !stunList || saveWhy === "stun"
                ? tr(
                    "pt_set_stun_bad",
                    { n: MAX_STUN },
                    `One host:port per line, at most ${MAX_STUN}.`,
                  )
                : undefined
            }
            className="font-mono text-xs"
            data-testid="pt-set-stun"
          />
          <div className="flex items-center gap-2">
            <Button
              size="sm"
              variant="secondary"
              disabled={!advancedChanged || hubBad || !hub.trim() || !stunList}
              onClick={() =>
                void save({ hub_url: hub.trim(), stun: stunList ?? [] })
              }
              data-testid="pt-set-save"
            >
              {tr("save", undefined, "Save")}
            </Button>
            {saved && (
              <span
                className="text-xs text-[var(--color-good)]"
                data-testid="pt-set-saved"
              >
                {tr("saved", undefined, "Saved")}
              </span>
            )}
          </div>
        </div>
      </details>
      {saveWhy && saveWhy !== "hub_url" && saveWhy !== "stun" && (
        <p
          className="text-xs text-[var(--color-warn)]"
          data-testid="pt-set-failed"
        >
          {tr(
            "pt_set_failed",
            undefined,
            "The engine didn't take this setting. Try again.",
          )}{" "}
          ({saveWhy})
        </p>
      )}
    </div>
  );
}
