import { useEffect, useRef, useState } from "react";

import {
  Button,
  Input,
  SegmentedControl,
  Textarea,
  Toggle,
} from "../../components";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import {
  advancedBody,
  hubUrlOk,
  invitesBody,
  joinApi,
  MAX_STUN,
  ownPagesNeedOwnHub,
  pageBaseOf,
  parseStun,
  portOk,
  type NetSettings,
  type NetSettingsBody,
  type NetSource,
} from "../Play/playTogetherState";
/**
 * Settings › App › Play Together: where invites work. Anywhere: one link (through the signalling
 * hub, the friend's browser connecting straight to this computer over WebRTC, staying on the Wi-Fi
 * when the friend is on it); Only on my Wi-Fi: the home network's link. The engine keeps one set of
 * these for every console, so any console's address reads and writes it. Which PS5 profile each
 * guest plays as is one console's (Settings › This PS5, PlayTogetherGuestProfiles).
 */
export function PlayTogetherSettings() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host) ?? "";
  const [net, setNet] = useState<NetSettings | null>(null);
  const [unread, setUnread] = useState(false);
  const [hub, setHub] = useState("");
  const [pageBase, setPageBase] = useState("");
  const [stun, setStun] = useState("");
  const [port, setPort] = useState("");
  const [portSaved, setPortSaved] = useState(false);
  const [saveWhy, setSaveWhy] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // What the text fields were last filled with. This read runs again each time Settings is
  // shown again: a field still as it was filled takes the saved value, one being typed in is
  // left alone (it used to be put back to the saved value, losing what was typed).
  const filled = useRef({ hub: "", pageBase: "", stun: "", port: "" });
  useEffect(() => {
    let alive = true;
    void joinApi.read(host).then((s) => {
      if (!alive) return;
      if (!s) {
        setUnread(true);
        return;
      }
      setNet(s.settings);
      const was = filled.current;
      const now = {
        hub: s.settings.hubUrl,
        pageBase: s.settings.pageBase ?? "",
        stun: s.settings.stun.join("\n"),
        port: s.settings.port ? String(s.settings.port) : "",
      };
      filled.current = now;
      setHub((cur) => (cur === was.hub ? now.hub : cur));
      setPageBase((cur) => (cur === was.pageBase ? now.pageBase : cur));
      setStun((cur) => (cur === was.stun ? now.stun : cur));
      setPort((cur) => (cur === was.port ? now.port : cur));
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
      const n = r.state.settings;
      setNet(n);
      if (body.hub_url != null || body.page_base != null || body.stun != null || body.reset) {
        setSaved(true);
        // The fields show what is in force now (an emptied one is its default again).
        const now = { ...filled.current, hub: n.hubUrl, pageBase: n.pageBase ?? "", stun: n.stun.join("\n") };
        filled.current = now;
        setHub(now.hub);
        setPageBase(now.pageBase);
        setStun(now.stun);
      }
    }
    return r.error ?? null;
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

  const invites =
    net.invites ?? (net.internetInvites ? "anywhere" : "wifi");
  const hubBad = hub.trim() !== "" && !hubUrlOk(hub);
  const pageBaseBad = pageBase.trim() !== "" && pageBaseOf(pageBase) === null;
  const stunList = parseStun(stun);
  const portNum = Number(port.trim());
  const portBad = port.trim() !== "" && !portOk(portNum);
  const portChanged = port.trim() !== "" && portNum !== (net.port ?? 0);
  const advanced = advancedBody(net, { hub, pageBase, stun });
  const sources = net.sources;
  const anyCustom = !!sources && Object.values(sources).some((v) => v === "custom");
  const sourceText = (v: NetSource | undefined) =>
    v === "custom"
      ? tr("pt_set_source_custom", undefined, "Your setting")
      : v === "environment"
        ? tr("pt_set_source_environment", undefined, "Set on the engine's server")
        : v === "default"
          ? tr("pt_set_source_default", undefined, "Default")
          : undefined;
  const needOwnHub = net.pageBase !== null && ownPagesNeedOwnHub(net.pageBase, net.hubUrl);

  return (
    <div className="space-y-3" data-testid="pt-settings">
      <div className="space-y-1.5" data-testid="pt-set-internet">
        <p id="pt-set-invites-label" className="text-sm font-medium">
          {tr("pt_set_invites", undefined, "Invites work")}
        </p>
        <SegmentedControl
          ariaLabel={tr("pt_set_invites", undefined, "Invites work")}
          value={invites}
          onChange={(v) => {
            const anywhere = v === "anywhere";
            setNet({
              ...net,
              internetInvites: anywhere,
              invites: anywhere ? "anywhere" : "wifi",
            });
            // Both names: an engine from before `invites` reads internet_invites.
            void save(invitesBody(anywhere ? "anywhere" : "wifi"));
          }}
          segments={[
            {
              value: "anywhere",
              label: tr(
                "pt_set_invites_anywhere",
                undefined,
                "Anywhere (recommended)",
              ),
            },
            {
              value: "wifi",
              label: tr("pt_set_invites_wifi", undefined, "Only on my Wi-Fi"),
            },
          ]}
        />
        <p
          className="text-xs text-[var(--color-muted)]"
          data-testid="pt-set-invites-hint"
        >
          {invites === "anywhere"
            ? tr(
                "pt_set_invites_anywhere_hint",
                undefined,
                "One link for every friend: on your Wi-Fi it stays on your Wi-Fi, elsewhere it goes over the internet. With no internet, the link works on your Wi-Fi.",
              )
            : tr(
                "pt_set_invites_wifi_hint",
                undefined,
                "Links work only for friends on your Wi-Fi (or your Tailscale network). Nothing goes through the invite service.",
              )}
        </p>
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
      {net.port !== null && (
        <div className="space-y-1.5" data-testid="pt-set-port">
          <div className="flex items-end gap-2">
            <Input
              label={tr("pt_set_port", undefined, "Port for friends on other networks (UDP)")}
              value={port}
              inputMode="numeric"
              onChange={(e) => {
                setPort(e.target.value.replace(/[^0-9]/g, "").slice(0, 5));
                setPortSaved(false);
              }}
              error={
                portBad || saveWhy === "port"
                  ? tr("pt_set_port_bad", undefined, "Use a number from 1024 to 65535.")
                  : undefined
              }
              className="w-28 font-mono"
              data-testid="pt-set-port-input"
            />
            <Button
              size="sm"
              variant="secondary"
              disabled={!portChanged || portBad}
              onClick={() =>
                void save({ port: portNum }).then((err) => setPortSaved(!err))
              }
              data-testid="pt-set-port-save"
            >
              {tr("save", undefined, "Save")}
            </Button>
          </div>
          <p className="text-xs text-[var(--color-muted)]" data-testid="pt-set-port-hint">
            {portSaved
              ? tr(
                  "pt_set_port_saved",
                  undefined,
                  "Saved. It takes effect the next time PS5Upload starts.",
                )
              : tr(
                  "pt_set_port_hint",
                  undefined,
                  "Stays the same every time. If your router can't open a port by itself, forward this UDP port to this computer in your router's settings.",
                )}
          </p>
        </div>
      )}
      <div data-testid="pt-set-video-track">
        <Toggle
          checked={net.videoTrack}
          onChange={(on) => {
            setNet({ ...net, videoTrack: on });
            void save({ video_track: on });
          }}
          label={tr("pt_set_video_track", undefined, "Video track for internet friends")}
          hint={tr(
            "pt_set_video_track_hint",
            undefined,
            "Sends the picture the way video calls do, so a lost packet costs a resend instead of a frozen picture. Off: the picture goes with the controls and the sound, as before.",
          )}
        />
      </div>
      <details className="text-sm" data-testid="pt-set-advanced">
        <summary className="cursor-pointer select-none text-xs text-[var(--color-muted)] hover:text-[var(--color-text)]">
          {tr("pt_set_advanced", undefined, "Advanced")}
        </summary>
        <div className="mt-3 space-y-3">
          {net.builtInHub && (
            <p
              className="text-xs text-[var(--color-muted)]"
              data-testid="pt-set-built-in-hub"
            >
              {net.builtInHub.publicUrl
                ? tr(
                    "pt_set_built_in_hub",
                    { port: net.builtInHub.port, url: net.builtInHub.publicUrl },
                    `This engine runs its own hub on port ${net.builtInHub.port}, reached at ${net.builtInHub.publicUrl}. It is set on the engine's server and can't be changed here.`,
                  )
                : tr(
                    "pt_set_built_in_hub_no_url",
                    { port: net.builtInHub.port },
                    `This engine runs its own hub on port ${net.builtInHub.port}, with no public address set. It is set on the engine's server and can't be changed here.`,
                  )}
            </p>
          )}
          <Input
            label={tr("pt_set_hub", undefined, "Hub address")}
            labelHint={sourceText(sources?.hubUrl)}
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
          {net.pageBase !== null && (
            <Input
              label={tr("pt_set_page_base", undefined, "Guest page address")}
              labelHint={sourceText(sources?.pageBase)}
              value={pageBase}
              onChange={(e) => {
                setPageBase(e.target.value);
                setSaved(false);
              }}
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
              hint={tr(
                "pt_set_page_base_hint",
                undefined,
                "Where your friends' links open. Change it only if you host the pages yourself: they are at join/, pad/ and share/ below this address.",
              )}
              error={
                pageBaseBad || saveWhy === "page_base"
                  ? tr(
                      "pt_set_page_base_bad",
                      undefined,
                      "Use an https:// address with no ? or # in it.",
                    )
                  : undefined
              }
              data-testid="pt-set-page-base"
            />
          )}
          {needOwnHub && (
            <p
              className="text-xs text-[var(--color-warn)]"
              data-testid="pt-set-own-hub"
            >
              {tr(
                "pt_set_page_base_needs_hub",
                undefined,
                "The default hub only accepts its own guest pages and the PS5Upload app, so friends who open your link in a browser can't join. Set the hub address to your own hub too.",
              )}
            </p>
          )}
          <Textarea
            label={tr("pt_set_stun", undefined, "STUN servers")}
            labelHint={sourceText(sources?.stun)}
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
              disabled={!advanced || hubBad || pageBaseBad || !stunList}
              onClick={() => {
                if (advanced) void save(advanced);
              }}
              data-testid="pt-set-save"
            >
              {tr("save", undefined, "Save")}
            </Button>
            {anyCustom && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void save({ reset: ["hub_url", "page_base", "stun"] })}
                data-testid="pt-set-reset"
              >
                {tr("pt_set_use_defaults", undefined, "Use the defaults")}
              </Button>
            )}
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
      {saveWhy && saveWhy !== "hub_url" && saveWhy !== "page_base" && saveWhy !== "stun" && saveWhy !== "port" && (
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
