// Remote Play on a real network: the latency readout, the low-latency and lower-quality
// settings, and the offer to drop to 720p when the network keeps losing frames.

import { useEffect, useRef, useState } from "react";
import { Wifi, X } from "lucide-react";

import { Spinner, Toggle } from "../../components";
import { BusyNetworkWatch, canStepDown } from "../../lib/rpAutoQuality";
import type { StreamStats } from "../../lib/remotePlayStream";
import { STAGES, stageValues } from "../../lib/rpLatency";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useRpQualityStore, wasOffered } from "../../state/rpQuality";

/** What the delay that can be measured comes to, ms: the round trip to the engine (null until
 *  it answers) and the time a frame takes from arriving to being drawn. The console's own leg
 *  to the engine has no clock both ends share, so it is not in here. */
export function latencyParts(s: Pick<StreamStats, "rttMs" | "decodeMs">): { engine: number | null; decode: number } {
  return {
    engine: s.rttMs === null ? null : Math.round(s.rttMs),
    decode: Math.max(0, Math.round(s.decodeMs)),
  };
}

/** The stats overlay's latency line, and under it each frame's stages (medians over the last
 *  2 s, measured: see lib/rpLatency.ts) once a frame was timed, short enough for a phone. */
export function LatencyLine({ stats }: { stats: StreamStats }) {
  const tr = useTr();
  const { engine, decode } = latencyParts(stats);
  const timed = !!stats.stages && STAGES.some((k) => stats.stages?.[k]);
  const v = stageValues(stats.stages);
  const restarts = stats.decoderRestarts ?? 0;
  return (
    <>
      <div data-testid="rp-latency">
        {engine === null
          ? tr("rp_stats_latency_decode", { decode }, `Latency ≈ ${decode} ms decode`)
          : tr("rp_stats_latency", { engine, decode }, `Latency ≈ ${engine} ms engine + ${decode} ms decode`)}
      </div>
      {(timed || restarts > 0) && (
        <div data-testid="rp-latency-stages">
          {tr("rp_stats_stages", v, `net ${v.net} · queue ${v.queue} · decode ${v.decode} · screen ${v.draw} ms`)}
          {restarts > 0 && (
            <>
              {" · "}
              {tr("rp_stats_decoder_restarts", { n: restarts }, `decoder restarts ${restarts}`)}
            </>
          )}
        </div>
      )}
    </>
  );
}

/** Low latency and Lower quality on a busy network, in the player's settings. */
export function RpNetworkSettings() {
  const tr = useTr();
  const prefs = useRemotePlayStore((s) => s.prefs);
  const setPrefs = useRemotePlayStore((s) => s.setPrefs);
  return (
    <>
      <Toggle
        checked={prefs.lowLatency}
        onChange={(v) => setPrefs({ lowLatency: v })}
        label={tr("rp_low_latency", undefined, "Low latency")}
        hint={tr(
          "rp_low_latency_hint",
          undefined,
          "The smallest sound buffer and the shortest picture queue, for the least delay. On a busy network more frames are dropped and the sound can crackle. The picture part applies the next time you start.",
        )}
      />
      <Toggle
        checked={prefs.autoQuality}
        onChange={(v) => setPrefs({ autoQuality: v })}
        label={tr("rp_auto_quality", undefined, "Lower quality on a busy network")}
        hint={tr(
          "rp_auto_quality_hint",
          undefined,
          "When a 1080p stream keeps losing frames, offers to switch to 720p. Switching restarts the stream, which takes about 15 s.",
        )}
      />
    </>
  );
}

/** Presses on it never reach the picture under it. */
const keep = {
  onClick: (e: React.MouseEvent) => e.stopPropagation(),
  onDoubleClick: (e: React.MouseEvent) => e.stopPropagation(),
  onPointerDown: (e: React.PointerEvent) => e.stopPropagation(),
  onKeyDown: (e: React.KeyboardEvent) => e.stopPropagation(),
  onKeyUp: (e: React.KeyboardEvent) => e.stopPropagation(),
};

/** On the picture: watches the stream's loss, offers 720p once when it stays bad at 1080p, and
 *  shows the switch while it waits for the PS5. Offered, not done on its own: a restart blacks
 *  the game out for ~15 s, which only the player can judge worth it at that moment. */
export function QualityNotice() {
  const tr = useTr();
  const stream = useRemotePlayStore((s) => s.stream);
  const phase = useRemotePlayStore((s) => s.phase);
  const stats = useRemotePlayStore((s) => s.stats);
  const enabled = useRemotePlayStore((s) => s.prefs.autoQuality);
  const res = useRemotePlayStore((s) => s.prefs.res);
  const offerFor = useRpQualityStore((s) => s.offerFor);
  const switching = useRpQualityStore((s) => s.switching);
  const { offer, dismiss, switchTo720, cancelSwitch } = useRpQualityStore.getState();

  const watch = useRef(new BusyNetworkWatch());
  useEffect(() => watch.current.reset(), [stream]);
  useEffect(() => {
    if (!stream || !stats || phase !== "playing") return;
    const can = canStepDown({ enabled, res, pictureHeight: stream.canvas.height, offered: wasOffered(stream) });
    if (can && watch.current.push(stats)) offer(stream);
    // Judged once a second, when the stats come.
  }, [stats]); // eslint-disable-line react-hooks/exhaustive-deps

  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!switching?.retryAt) return;
    const t = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(t);
  }, [switching?.retryAt]);

  if (switching) {
    const left = switching.retryAt ? Math.max(1, Math.ceil((switching.retryAt - now) / 1000)) : null;
    return (
      <div
        role="status"
        className="absolute left-1/2 top-1/2 z-30 flex w-[min(24rem,calc(100%-2rem))] -translate-x-1/2 -translate-y-1/2 flex-col items-center gap-2 rounded-2xl bg-black/80 px-4 py-3 text-center text-sm text-white"
        {...keep}
        data-testid="rp-quality-switching"
      >
        <span className="flex items-center gap-2 font-medium">
          <Spinner size={14} tone="inherit" />
          {tr("rp_switching", undefined, "Switching to 720p…")}
        </span>
        {left !== null && (
          <span className="text-xs text-white/70">
            {tr("rp_switching_wait", { s: left }, `The PS5 is freeing its last session: about ${left} s.`)}
          </span>
        )}
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            cancelSwitch();
          }}
          className="rounded-full bg-white/15 px-3 py-1 text-xs hover:bg-white/25"
        >
          {tr("rp_switch_cancel", undefined, "Cancel")}
        </button>
      </div>
    );
  }
  if (!stream || offerFor !== stream || phase !== "playing") return null;
  return (
    <div
      role="alert"
      className="absolute left-1/2 z-30 flex w-max max-w-[calc(100%-2rem)] -translate-x-1/2 items-center gap-2 rounded-full bg-black/80 py-1.5 pe-1.5 ps-3 text-sm text-white shadow-lg"
      style={{ top: "calc(max(0.75rem, env(safe-area-inset-top)) + 2.75rem)" }}
      {...keep}
      data-testid="rp-quality-offer"
    >
      <Wifi size={15} className="shrink-0 text-white/70" aria-hidden />
      <span className="min-w-0 truncate">{tr("rp_busy_net", undefined, "The network is dropping frames.")}</span>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          void switchTo720();
        }}
        className="shrink-0 rounded-full bg-white px-3 py-1 text-xs font-medium text-black hover:bg-white/90"
        data-testid="rp-quality-switch"
      >
        {tr("rp_busy_net_switch", undefined, "Switch to 720p")}
      </button>
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          dismiss();
        }}
        aria-label={tr("rp_busy_net_dismiss", undefined, "Not now")}
        className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full hover:bg-white/20"
      >
        <X size={14} />
      </button>
    </div>
  );
}
