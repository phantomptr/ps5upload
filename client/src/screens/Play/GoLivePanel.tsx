import { useEffect, useMemo, useRef, useState } from "react";
import { Check, Copy, Download, ExternalLink, Plus, Radio, Trash2, X } from "lucide-react";

import { Button, Callout, Input, ProgressBar, Toggle } from "../../components";
import { writeClipboard } from "../../lib/clipboard";
import {
  draftProblem,
  emptyDraft,
  formatElapsed,
  formatKbps,
  formatMode,
  formatRate,
  isLive,
  liveFeedUrl,
  playFix,
  uploadNeededKbps,
  type DestinationDraft,
  type DraftProblem,
  type LiveCheck,
  type LiveDestination,
  type LiveFeedStatus,
  type LiveRun,
  type PresetId,
  type StreamStatus,
} from "../../lib/goLive";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { isMobile } from "../../lib/platform";
import { getEngineUrl, isLoopbackUrl } from "../../state/engine";
import { useGoLiveStore } from "../../state/goLive";
import { useTr } from "../../state/lang";
import { presetName, problemText, runSite } from "./goLiveText";
import { useRemotePlayStore } from "../../state/remotePlay";
import { useRpQualityStore } from "../../state/rpQuality";
import { PictureHereToggle } from "./PictureHere";

type Tr = ReturnType<typeof useTr>;

export { presetName, problemText };

/** Go live runs in the engine: with ffmpeg on a computer (the desktop app, and the web UI on any
 *  device, a phone's Safari included, #731), and in the phone apps by the app's own engine with
 *  the phone's sound converter, no ffmpeg (#763). The status says when a phone can't. */
export function goLiveAvailable(_p: { tauri: boolean; mobile: boolean }): boolean {
  return true;
}

/** The engine needs ffmpeg for this status (not when it streams by itself). */
export function needsFfmpeg(status: Pick<StreamStatus, "pipeline">): boolean {
  return (status.pipeline ?? "ffmpeg") === "ffmpeg";
}

/** The phone app (Tauri on Android/iOS): its engine always runs inside it, on the phone. */
export function inAppPhoneEngine(p: { tauri: boolean; mobile: boolean }): boolean {
  return p.tauri && p.mobile;
}

/** The OBS feed answers only on the engine's own computer, so it is offered where OBS can be
 *  on the same machine: an engine on this device (desktop app, or the web UI opened there). */
export function obsFeedAvailable(p: { mobile: boolean; engineUrl: string }): boolean {
  return !p.mobile && isLoopbackUrl(p.engineUrl);
}

const PRESET_ORDER: PresetId[] = ["youtube", "twitch", "kick", "facebook", "custom"];

/** Each site's colour, for its dot. */
const PRESET_COLOR: Record<PresetId, string> = {
  youtube: "#ff0033",
  twitch: "#9146ff",
  kick: "#53fc18",
  facebook: "#0866ff",
  custom: "#8b8b8b",
};

/** Where the key is, per site. */
function keyHelp(tr: Tr, id: PresetId): string {
  switch (id) {
    case "youtube":
      return tr(
        "golive_help_youtube",
        undefined,
        "YouTube Studio › Create › Go live › Stream: copy the Stream key. A channel's first live stream can only start once YouTube has turned live streaming on, which takes up to 24 hours.",
      );
    case "twitch":
      return tr("golive_help_twitch", undefined, "Twitch Creator Dashboard › Settings › Stream: copy the Primary Stream key.");
    case "kick":
      return tr(
        "golive_help_kick",
        undefined,
        "Kick Dashboard › Settings › Stream URL & Key: copy the Stream URL into Server and the Stream Key into Key.",
      );
    case "facebook":
      return tr(
        "golive_help_facebook",
        undefined,
        "Facebook Live Producer › Create live video › Streaming software: copy the Stream key.",
      );
    default:
      return tr(
        "golive_help_custom",
        undefined,
        "The server address from your streaming service or your own server (rtmp://, rtmps:// or srt://), and its stream key if it has one.",
      );
  }
}

/** How to try it privately, per site. */
function testTip(tr: Tr, id: PresetId): string | null {
  switch (id) {
    case "youtube":
      return tr("golive_test_youtube", undefined, "To try it privately, set the stream to Unlisted or Private in YouTube Studio first.");
    case "twitch":
      return tr(
        "golive_test_twitch",
        undefined,
        "Test without going public sends the stream to Twitch without showing it to anyone (a bandwidth test).",
      );
    case "facebook":
      return tr("golive_test_facebook", undefined, "To try it privately, choose Only me as the audience in Live Producer.");
    default:
      return null;
  }
}

function draftProblemText(tr: Tr, p: DraftProblem, preset: PresetId): string {
  switch (p) {
    case "key_missing":
      return tr("golive_err_key_missing", undefined, "Paste the stream key from the site.");
    case "key_invalid":
      return tr("golive_err_key_invalid", undefined, "That doesn't look like a stream key: paste only the key, without spaces.");
    case "server_missing":
      return tr("golive_err_server_missing", undefined, "Paste the Server URL from your Kick dashboard.");
    default:
      return preset === "custom"
        ? tr("golive_err_url_custom", undefined, "Enter a server address starting with rtmp://, rtmps:// or srt://.")
        : tr("golive_err_url", undefined, "Enter the server address starting with rtmps://.");
  }
}

/** A site's limit check in the user's language. */
export function checkText(tr: Tr, c: LiveCheck, site: string): string {
  const mbps = (k: unknown) => String(Math.round((Number(k) / 1000) * 10) / 10);
  switch (c.code) {
    case "hdr":
      return tr("golive_check_hdr", { site }, `Remote Play is in HDR, and ${site} would show its colours wrong. Restart Play in H.264 to go live.`);
    case "codec":
      return tr(
        "golive_check_codec",
        { site },
        `${site} takes H.264 only, and Remote Play is sending HEVC. Restart Play in H.264 to go live there.`,
      );
    case "bitrate":
      return tr(
        "golive_check_bitrate",
        { have: mbps(c.have), site, limit: mbps(c.limit) },
        `Your Play bitrate ${mbps(c.have)} Mbps is above ${site}'s ${mbps(c.limit)} Mbps. Lower it in Play settings (Picture) before going live.`,
      );
    case "height":
      return tr("golive_check_height", { site, limit: String(c.limit) }, `${site} takes at most ${String(c.limit)}p.`);
    default:
      return tr("golive_check_fps", { site, limit: String(c.limit) }, `${site} takes at most ${String(c.limit)} fps.`);
  }
}


function SiteDot({ id }: { id: PresetId }) {
  return <span aria-hidden className="inline-block h-2.5 w-2.5 shrink-0 rounded-full" style={{ background: PRESET_COLOR[id] }} />;
}

/** The first-time download of ffmpeg. */
function FfmpegPrompt({ status }: { status: StreamStatus }) {
  const tr = useTr();
  const install = useGoLiveStore((s) => s.installFfmpeg);
  const f = status.ffmpeg;
  const mb = String(Math.round((f.download_bytes ?? 60_000_000) / 1_000_000));
  if (!f.supported) {
    return (
      <Callout
        tone="warn"
        title={
          f.needs_own
            ? tr(
                "golive_ffmpeg_needs_own",
                undefined,
                "The ffmpeg download can't run on this system (a Docker image, for example). To go live here, give the engine your own ffmpeg with PS5UPLOAD_FFMPEG. The FAQ says how.",
              )
            : tr("golive_ffmpeg_unsupported", undefined, "Going live isn't available on this computer's system.")
        }
      />
    );
  }
  const st = f.install;
  const pct = st.total_bytes ? Math.round((st.done_bytes / st.total_bytes) * 100) : 0;
  return (
    <div className="space-y-2 rounded-md border border-[var(--color-border)] p-3" data-testid="golive-ffmpeg">
      <p className="text-sm">
        {tr(
          "golive_ffmpeg_ask",
          { mb },
          `PS5Upload needs a free helper (ffmpeg, about ${mb} MB) to convert the sound for streaming sites. Download it?`,
        )}
      </p>
      {st.running ? (
        <div className="space-y-1">
          <ProgressBar value={st.step === "downloading" ? pct / 100 : null} />
          <p className="text-xs text-[var(--color-muted)]" data-testid="golive-ffmpeg-progress">
            {st.step === "downloading"
              ? tr("golive_ffmpeg_downloading", { pct: String(pct) }, `Downloading… ${pct}%`)
              : st.step === "verifying"
                ? tr("golive_ffmpeg_verifying", undefined, "Checking the download…")
                : tr("golive_ffmpeg_unpacking", undefined, "Unpacking…")}
          </p>
        </div>
      ) : (
        <Button size="sm" variant="primary" leftIcon={<Download size={14} />} onClick={() => void install()} data-testid="golive-ffmpeg-download">
          {tr("golive_ffmpeg_download", undefined, "Download")}
        </Button>
      )}
      {st.error && !st.running && (
        <p className="text-xs text-[var(--color-danger,#dc2626)]" role="alert">
          {problemText(tr, st.error, st.message ?? st.error)}
        </p>
      )}
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "golive_ffmpeg_note",
          undefined,
          "It is downloaded once, checked against a fixed fingerprint before it is used, and kept with PS5Upload's data.",
        )}
      </p>
    </div>
  );
}

/** Adding a site and its key. */
function AddForm({ preset, onDone }: { preset: PresetId; onDone: () => void }) {
  const tr = useTr();
  const add = useGoLiveStore((s) => s.add);
  const busy = useGoLiveStore((s) => s.busy);
  const err = useGoLiveStore((s) => s.actionError);
  const keyPage = useGoLiveStore((s) => s.status?.presets.find((p) => p.id === preset)?.key_page ?? "");
  const [d, setD] = useState<DestinationDraft>(() => emptyDraft(preset));
  const [touched, setTouched] = useState(false);
  const problem = draftProblem(d);
  const name = presetName(tr, preset);
  const tip = testTip(tr, preset);
  const save = async () => {
    setTouched(true);
    if (problem) return;
    if (await add(d)) onDone();
  };
  return (
    <form
      className="space-y-2 rounded-md border border-[var(--color-border)] p-3"
      data-testid={`golive-add-${preset}`}
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <div className="flex items-center gap-2 text-sm font-medium">
        <SiteDot id={preset} />
        {tr("golive_add_title", { site: name }, `Add ${name}`)}
      </div>
      <p className="text-xs text-[var(--color-muted)]" data-testid="golive-key-help">
        {keyHelp(tr, preset)}{" "}
        {keyPage && (
          <button
            type="button"
            className="inline-flex items-center gap-1 underline"
            onClick={() => void openExternalUrl(keyPage)}
          >
            {tr("golive_open_site", { site: name }, `Open ${name}`)}
            <ExternalLink size={11} />
          </button>
        )}
      </p>
      {(preset === "kick" || preset === "custom") && (
        <Input
          label={preset === "kick" ? tr("golive_server", undefined, "Server") : tr("golive_url", undefined, "Server address")}
          value={d.server}
          placeholder={preset === "kick" ? "rtmps://….global-contribute.live-video.net/app" : "rtmp://… / rtmps://… / srt://…"}
          onChange={(e) => setD({ ...d, server: e.target.value })}
          autoComplete="off"
          spellCheck={false}
          data-testid="golive-server"
        />
      )}
      <Input
        label={
          preset === "custom"
            ? tr("golive_key_optional", undefined, "Stream key (if it has one)")
            : tr("golive_key", undefined, "Stream key")
        }
        type="password"
        value={d.key}
        onChange={(e) => setD({ ...d, key: e.target.value })}
        autoComplete="off"
        spellCheck={false}
        error={touched && problem ? draftProblemText(tr, problem, preset) : undefined}
        hint={tr("golive_key_private", undefined, "Kept encrypted on this computer and never shown again: only its last 4 characters.")}
        data-testid="golive-key"
      />
      {preset === "twitch" && (
        <Toggle
          checked={d.bandwidth_test}
          onChange={(v) => setD({ ...d, bandwidth_test: v })}
          label={tr("golive_twitch_test", undefined, "Test without going public")}
          hint={tip ?? undefined}
        />
      )}
      {preset !== "twitch" && tip && <p className="text-xs text-[var(--color-muted)]">{tip}</p>}
      {err && touched && (
        <p className="text-xs text-[var(--color-danger,#dc2626)]" role="alert">
          {problemText(tr, err.code, err.message, name)}
        </p>
      )}
      <div className="flex gap-2">
        <Button size="sm" variant="primary" type="submit" disabled={busy} data-testid="golive-add-save">
          {tr("golive_add_save", undefined, "Save")}
        </Button>
        <Button size="sm" variant="ghost" type="button" onClick={onDone}>
          {tr("golive_add_cancel", undefined, "Cancel")}
        </Button>
      </div>
    </form>
  );
}

function DestinationRow({ d, disabled }: { d: LiveDestination; disabled: boolean }) {
  const tr = useTr();
  const setEnabled = useGoLiveStore((s) => s.setEnabled);
  const setTest = useGoLiveStore((s) => s.setBandwidthTest);
  const remove = useGoLiveStore((s) => s.remove);
  const site = presetName(tr, d.preset);
  return (
    <li className="space-y-1 rounded-md border border-[var(--color-border)] p-2" data-testid="golive-dest">
      <div className="flex items-center gap-2">
        <input
          type="checkbox"
          className="h-5 w-5 shrink-0 sm:h-auto sm:w-auto"
          checked={d.enabled}
          disabled={disabled}
          onChange={(e) => void setEnabled(d.id, e.target.checked)}
          aria-label={tr("golive_use", { name: d.label }, `Stream to ${d.label}`)}
          data-testid="golive-dest-enabled"
        />
        <SiteDot id={d.preset} />
        <span className="min-w-0 flex-1 truncate text-sm">
          {d.label}
          {d.label !== site && <span className="text-[var(--color-muted)]"> · {site}</span>}
        </span>
        {d.key_hint && (
          <span className="font-mono text-xs text-[var(--color-muted)]" title={tr("golive_key_end", undefined, "The key's last characters")}>
            {d.key_hint}
          </span>
        )}
        <button
          type="button"
          disabled={disabled}
          onClick={() => void remove(d.id)}
          aria-label={tr("golive_remove", { name: d.label }, `Remove ${d.label}`)}
          className="flex h-10 w-10 shrink-0 items-center justify-center rounded hover:bg-[var(--color-surface-3)] disabled:opacity-40 sm:h-7 sm:w-7"
          data-testid="golive-dest-remove"
        >
          <Trash2 size={14} />
        </button>
      </div>
      {d.key_lost && (
        <p className="ps-6 text-xs text-[var(--color-danger,#dc2626)]" data-testid="golive-key-lost">
          {tr("golive_key_lost", undefined, "The saved stream key can't be read. Remove this site and add it again.")}
        </p>
      )}
      {d.preset === "twitch" && (
        <label className="flex min-h-10 items-center gap-2 ps-6 text-xs sm:min-h-0">
          <input
            type="checkbox"
            className="h-5 w-5 shrink-0 sm:h-auto sm:w-auto"
            checked={d.bandwidth_test}
            disabled={disabled}
            onChange={(e) => void setTest(d.id, e.target.checked)}
            data-testid="golive-dest-test"
          />
          {tr("golive_twitch_test", undefined, "Test without going public")}
        </label>
      )}
      {d.enabled &&
        (d.checks ?? []).map((c) => (
          <p
            key={c.code}
            className={`ps-6 text-xs ${c.blocking ? "text-[var(--color-danger,#dc2626)]" : "text-[var(--color-warn)]"}`}
            data-testid={`golive-check-${c.code}`}
          >
            {checkText(tr, c, site)}
          </p>
        ))}
    </li>
  );
}

const STATE_COLOR: Record<string, string> = {
  live: "bg-[var(--color-danger,#dc2626)]",
  connecting: "bg-[var(--color-muted)]",
  retrying: "bg-[var(--color-warn)]",
  failed: "bg-[var(--color-muted)]",
};

/** While live: how it goes, per site, and Stop. */
function LiveView({ run }: { run: LiveRun }) {
  const tr = useTr();
  const stop = useGoLiveStore((s) => s.stop);
  const busy = useGoLiveStore((s) => s.busy);
  const stopping = run.phase === "stopping";
  const stateText = (s: string) =>
    s === "live"
      ? tr("golive_state_live", undefined, "Live")
      : s === "connecting"
        ? tr("golive_state_connecting", undefined, "Connecting…")
        : s === "retrying"
          ? tr("golive_state_retrying", undefined, "Reconnecting…")
          : tr("golive_state_failed", undefined, "Stopped");
  return (
    <div className="space-y-3" data-testid="golive-live">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
        <LivePill run={run} />
        {run.phase === "live" && (
          <span className="text-[var(--color-muted)]">
            {formatRate(run.kbps, run.fps)}
          </span>
        )}
      </div>
      {run.phase === "reconnecting" && (
        <p className="text-xs text-[var(--color-warn)]" role="status" data-testid="golive-reconnecting">
          {run.error ? problemText(tr, run.error.code, run.error.message, runSite(tr, run)) + " " : ""}
          {/* The wait is over and the attempt is under way: not "again in 0 s". */}
          {run.retry_in_s
            ? tr("golive_retry_in", { s: String(run.retry_in_s) }, `Trying again in ${run.retry_in_s} s.`)
            : tr("golive_state_connecting", undefined, "Connecting…")}
        </p>
      )}
      {run.phase === "waiting_for_play" && (
        <p className="text-xs text-[var(--color-warn)]" role="status">
          {tr("golive_waiting_play", undefined, "Waiting for Remote Play to start again. The stream carries on once it does.")}
        </p>
      )}
      {run.slow_upload && (
        <p className="text-xs text-[var(--color-warn)]" role="status" data-testid="golive-slow">
          {tr(
            "golive_slow_upload",
            undefined,
            "This computer's upload can't keep up: viewers see skips. Lower the bitrate in Play settings (Picture), or stream to fewer sites.",
          )}
        </p>
      )}
      <ul className="space-y-1">
        {run.destinations.map((d) => (
          <li key={d.id} className="flex items-start gap-2 text-sm" data-testid="golive-live-dest">
            <span aria-hidden className={`mt-1.5 inline-block h-2 w-2 shrink-0 rounded-full ${STATE_COLOR[d.state] ?? ""}`} />
            <span className="min-w-0 flex-1">
              <span>{d.label}</span>
              <span className="text-[var(--color-muted)]"> · {stateText(d.state)}</span>
              {d.bandwidth_test && (
                <span className="text-[var(--color-muted)]"> · {tr("golive_test_mode", undefined, "test, not public")}</span>
              )}
              {d.error && d.state !== "live" && (
                <span className="block text-xs text-[var(--color-muted)]">{problemText(tr, d.error.code, d.error.message, presetName(tr, d.preset))}</span>
              )}
            </span>
          </li>
        ))}
      </ul>
      <Button size="sm" variant="danger" onClick={() => void stop()} disabled={busy || stopping} data-testid="golive-stop">
        {stopping ? tr("golive_state_stopping", undefined, "Stopping…") : tr("golive_stop", undefined, "Stop streaming")}
      </Button>
    </div>
  );
}

/** "● LIVE 12:34" (or Reconnecting / Starting). */
export function LivePill({ run }: { run: LiveRun }) {
  const tr = useTr();
  const live = run.phase === "live";
  // "LIVE" is the broadcast word everywhere, as on the sites themselves.
  const label = live
    ? "LIVE"
    : run.phase === "reconnecting"
      ? tr("golive_state_retrying", undefined, "Reconnecting…")
      : run.phase === "waiting_for_play"
        ? tr("golive_waiting_short", undefined, "Waiting for Play")
        : run.phase === "stopping"
          ? tr("golive_state_stopping", undefined, "Stopping…")
          : tr("golive_state_connecting", undefined, "Connecting…");
  return (
    <span
      className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-semibold text-white ${
        live ? "bg-[var(--color-danger,#dc2626)]" : "bg-black/60"
      }`}
      data-testid="golive-pill"
    >
      <span aria-hidden className={`inline-block h-2 w-2 rounded-full ${live ? "animate-pulse bg-white" : "bg-[var(--color-warn,#f59e0b)]"}`} />
      {label}
      {live && run.live_s !== null && <span className="font-mono">{formatElapsed(run.live_s)}</span>}
    </span>
  );
}

/** On the picture while Go live runs: the pill, and the bitrate. */
export function GoLiveChip() {
  const run = useGoLiveStore((s) => s.status?.live ?? null);
  if (!isLive(run) || !run) return null;
  return (
    <div className="pointer-events-none absolute end-3 top-14 z-20 flex items-center gap-2" data-testid="golive-chip">
      <LivePill run={run} />
      {run.phase === "live" && (
        <span className="rounded-full bg-black/60 px-2 py-0.5 text-xs text-white">{formatKbps(run.kbps)}</span>
      )}
    </div>
  );
}

function GoLiveTab({ status }: { status: StreamStatus }) {
  const tr = useTr();
  const start = useGoLiveStore((s) => s.start);
  const busy = useGoLiveStore((s) => s.busy);
  const err = useGoLiveStore((s) => s.actionError);
  const clearError = useGoLiveStore((s) => s.clearError);
  const restartWith = useRpQualityStore((s) => s.restartWith);
  const switching = useRpQualityStore((s) => !!s.switching);
  const [adding, setAdding] = useState<PresetId | null>(null);
  const run = status.live;
  const chosen = status.destinations.filter((d) => d.enabled);
  const fix = useMemo(() => playFix(status.session, chosen), [status.session, chosen]);
  const blocked = chosen.some((d) => (d.checks ?? []).some((c) => c.blocking));
  const ready = status.pipeline === "native" || (needsFfmpeg(status) && status.ffmpeg.installed);
  const needMbps = (uploadNeededKbps(status.session, chosen.length) / 1000).toFixed(1);

  if (isLive(run) && run) return <LiveView run={run} />;
  const lastFailed = run?.phase === "failed" ? run : null;
  return (
    <div className="space-y-3">
      {status.store_error && (
        <div data-testid="golive-store-error">
          <Callout tone="error" title={tr("golive_store_error_title", undefined, "Your saved sites can't be read")}>
            <p className="text-xs">
              {tr("golive_store_error_hint", undefined, "Fix the file named here or move it away. Go live tries again by itself.")}
            </p>
            <p className="break-all font-mono text-xs">{status.store_error}</p>
          </Callout>
        </div>
      )}
      {status.pipeline === "none" && (
        <div data-testid="golive-native-unavailable">
          <Callout
            tone="warn"
            title={tr("golive_native_unavailable", undefined, "This device can't convert the sound for streaming sites, so going live isn't available here.")}
          />
        </div>
      )}
      {status.pipeline === "native" && isMobile() && (
        <p className="text-xs text-[var(--color-muted)]" data-testid="golive-phone-note">
          {tr(
            "golive_phone_keep_open",
            undefined,
            "The phone sends the stream itself. Keep PS5Upload open on screen while you're live: the stream stops if the app goes to the background or the phone locks.",
          )}
        </p>
      )}
      {needsFfmpeg(status) && !status.ffmpeg.installed && <FfmpegPrompt status={status} />}
      {lastFailed?.error && (
        <div data-testid="golive-failed">
          <Callout tone="error" title={tr("golive_stopped", undefined, "The stream stopped")}>
            {problemText(tr, lastFailed.error.code, lastFailed.error.message, runSite(tr, lastFailed))}
          </Callout>
        </div>
      )}
      {status.destinations.length > 0 ? (
        <ul className="space-y-1.5" data-testid="golive-dests">
          {status.destinations.map((d) => (
            <DestinationRow key={d.id} d={d} disabled={busy} />
          ))}
        </ul>
      ) : (
        !adding && (
          <p className="text-sm text-[var(--color-muted)]">
            {tr("golive_none_yet", undefined, "Add where to stream: YouTube, Twitch, Kick, Facebook, or your own server.")}
          </p>
        )
      )}
      {adding ? (
        <AddForm
          key={adding}
          preset={adding}
          onDone={() => {
            setAdding(null);
            clearError();
          }}
        />
      ) : (
        <div className="flex flex-wrap gap-1.5" data-testid="golive-add-buttons">
          {PRESET_ORDER.map((p) => (
            <button
              key={p}
              type="button"
              className="chip inline-flex min-h-10 items-center gap-1.5 px-3 text-xs sm:min-h-8"
              onClick={() => {
                clearError();
                setAdding(p);
              }}
              data-testid={`golive-add-${p}-button`}
            >
              <Plus size={12} />
              <SiteDot id={p} />
              {presetName(tr, p)}
            </button>
          ))}
        </div>
      )}
      {fix && status.session && (
        <div data-testid="golive-fix">
        <Callout tone="warn" title={tr("golive_fix_title", undefined, "Play needs a restart for these sites")}>
          <p className="text-xs">
            {fix.codec && fix.bitrateKbps
              ? tr(
                  "golive_fix_both2",
                  { mbps: String(fix.bitrateKbps / 1000) },
                  `Restart Remote Play in H.264 at ${fix.bitrateKbps / 1000} Mbps? The game keeps running; the picture comes back in about 15 s. This lasts for this session only: your Picture settings stay as they are, and this asks again next time.`,
                )
              : fix.codec
                ? tr(
                    "golive_fix_codec2",
                    undefined,
                    "Restart Remote Play in H.264? The game keeps running; the picture comes back in about 15 s. This lasts for this session only: your Picture settings stay as they are, and this asks again next time.",
                  )
                : tr(
                    "golive_fix_bitrate2",
                    { mbps: String((fix.bitrateKbps ?? 0) / 1000) },
                    `Restart Remote Play at ${(fix.bitrateKbps ?? 0) / 1000} Mbps? The game keeps running; the picture comes back in about 15 s. This lasts for this session only: your Picture settings stay as they are, and this asks again next time.`,
                  )}
          </p>
          <Button
            size="sm"
            className="mt-2"
            disabled={switching}
            onClick={() =>
              void restartWith({
                ...(fix.codec ? { codec: "h264" as const } : {}),
                ...(fix.bitrateKbps ? { bitrateKbps: fix.bitrateKbps } : {}),
              })
            }
            data-testid="golive-fix-restart"
          >
            {switching ? tr("golive_fix_restarting", undefined, "Restarting Play…") : tr("golive_fix_restart", undefined, "Restart Play")}
          </Button>
        </Callout>
        </div>
      )}
      {err && !adding && (
        <p className="text-xs text-[var(--color-danger,#dc2626)]" role="alert" data-testid="golive-error">
          {problemText(tr, err.code, err.message)}
        </p>
      )}
      {status.destinations.length > 0 && (
        <div className="flex flex-wrap items-center gap-3">
          <Button
            variant="danger"
            size="md"
            leftIcon={<Radio size={15} />}
            disabled={busy || !chosen.length || !ready || blocked || !status.session}
            onClick={() => void start()}
            data-testid="golive-start"
          >
            {tr("golive_go", undefined, "Go live")}
          </Button>
          {status.session && chosen.length > 0 && (
            <span className="text-xs text-[var(--color-muted)]">
              {tr("golive_upload_need", { mbps: needMbps }, `Needs about ${needMbps} Mbps of upload.`)}
            </span>
          )}
        </div>
      )}
    </div>
  );
}

/** The OBS tab: the feed's address and the steps in OBS. */
export function ObsFeedView({
  url,
  status,
  copied,
  onCopy,
}: {
  url: string;
  status: LiveFeedStatus | null;
  copied: boolean;
  onCopy: () => void;
}) {
  const tr = useTr();
  const connected = !!status?.running && status.consumers > 0;
  const state = !status?.running
    ? tr("rp_obs_not_running", undefined, "Start Remote Play to send the feed.")
    : connected
      ? tr("rp_obs_connected", { n: String(status.consumers) }, `OBS connected (${status.consumers})`)
      : tr("rp_obs_waiting", undefined, "Waiting for OBS");
  const steps = [
    tr("rp_obs_step1", undefined, "In OBS: Sources › + › Media Source, and give it a name."),
    tr("rp_obs_step2", undefined, "Untick Local file. Input: paste the address above. Input format: mpegts."),
    tr("rp_obs_step3", undefined, "Tick Restart playback when source becomes active."),
    tr("rp_obs_step4", undefined, "Set Network Buffering to its lowest, then press OK."),
  ];
  return (
    <div className="space-y-3" data-testid="rp-obs">
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_obs_intro",
          undefined,
          "Send the picture and sound to OBS on this computer, to stream with your own scenes or to record. The console's video goes to OBS as it is, with nothing re-encoded here.",
        )}
      </p>
      <div className="flex items-center gap-2">
        <input
          readOnly
          value={url}
          onFocus={(e) => e.currentTarget.select()}
          aria-label={tr("rp_obs_address", undefined, "Address for OBS")}
          className="min-w-0 flex-1 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] px-2 py-1 font-mono text-xs"
          data-testid="rp-obs-url"
        />
        <Button size="sm" onClick={onCopy} leftIcon={copied ? <Check size={14} /> : <Copy size={14} />} data-testid="rp-obs-copy">
          {copied ? tr("rp_obs_copied", undefined, "Copied") : tr("rp_obs_copy", undefined, "Copy")}
        </Button>
      </div>
      <div className="flex items-center gap-2 text-xs" role="status" data-testid="rp-obs-state">
        <span aria-hidden className={`inline-block h-2 w-2 rounded-full ${connected ? "bg-[var(--color-good)]" : "bg-[var(--color-muted)]"}`} />
        <span>{state}</span>
        {status?.running && status.height && status.fps ? (
          <span className="text-[var(--color-muted)]">
            · {formatMode(status.height, status.fps)}
          </span>
        ) : null}
      </div>
      <ol className="list-decimal space-y-1 ps-5 text-xs">
        {steps.map((s, i) => (
          <li key={i}>{s}</li>
        ))}
      </ol>
      <p className="text-xs text-[var(--color-muted)]">
        {tr(
          "rp_obs_notes",
          undefined,
          "The feed has none of this app's buttons or overlays, and keeps going while this window is hidden or minimized. OBS sends it on at the bitrate set in OBS (Settings › Output); keep that within what your streaming site allows, for example about 6 Mbps on Twitch.",
        )}
      </p>
      {status?.running && status.codec === "h265_hdr" ? (
        <p className="text-xs text-[var(--color-warn)]">
          {tr("rp_obs_hdr", undefined, "HDR is on: OBS may show washed-out colours. Turn HDR off for streaming.")}
        </p>
      ) : null}
    </div>
  );
}

function ObsTab() {
  const engineUrl = getEngineUrl();
  const [status, setStatus] = useState<LiveFeedStatus | null>(null);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    let gone = false;
    const poll = async () => {
      try {
        const r = await fetch(`${engineUrl}/api/ps5/rp/live/status`);
        const v = r.ok ? ((await r.json()) as LiveFeedStatus) : null;
        if (!gone) setStatus(v);
      } catch {
        if (!gone) setStatus(null);
      }
    };
    void poll();
    const t = window.setInterval(() => void poll(), 2000);
    return () => {
      gone = true;
      window.clearInterval(t);
    };
  }, [engineUrl]);
  useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(t);
  }, [copied]);
  const url = liveFeedUrl(engineUrl);
  return <ObsFeedView url={url} status={status} copied={copied} onCopy={() => void writeClipboard(url).then((ok) => setCopied(ok))} />;
}

/** The Stream panel (Play stream bar › Stream): Go live and OBS. */
export function GoLivePanel({ onClose }: { onClose: () => void }) {
  const tr = useTr();
  const status = useGoLiveStore((s) => s.status);
  const loadError = useGoLiveStore((s) => s.loadError);
  const obs = obsFeedAvailable({ mobile: isMobile(), engineUrl: getEngineUrl() });
  const hasStream = useRemotePlayStore((s) => !!s.stream);
  const [tab, setTab] = useState<"live" | "obs">("live");
  // On a phone the panel opens from the menu under the picture, out of sight: bring it in.
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (typeof window !== "undefined" && window.matchMedia?.("(max-width: 639px)").matches)
      box.current?.scrollIntoView?.({ block: "start", behavior: "smooth" });
  }, []);
  return (
    <div
      ref={box}
      className="min-w-0 max-w-full space-y-3 break-words rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface-raised)] p-3 sm:p-4"
      data-testid="golive-panel"
    >
      <div className="flex items-center gap-2">
        <Radio size={16} />
        <h3 className="flex-1 text-sm font-semibold">{tr("golive_title", undefined, "Stream")}</h3>
        <button
          type="button"
          onClick={onClose}
          aria-label={tr("golive_close", undefined, "Close")}
          className="flex h-10 w-10 shrink-0 items-center justify-center rounded hover:bg-[var(--color-surface-3)] sm:h-7 sm:w-7"
        >
          <X size={16} />
        </button>
      </div>
      {hasStream && (
        <PictureHereToggle
          hint={tr(
            "golive_picture_hint",
            undefined,
            "Off, this computer sends the stream on without decoding or showing it, which saves CPU and heat. Keep the app running; you stay in control here.",
          )}
        />
      )}
      {obs && (
        <div className="flex gap-1.5" role="group" data-testid="golive-tabs">
          {(["live", "obs"] as const).map((t) => (
            <button
              key={t}
              type="button"
              aria-pressed={tab === t}
              onClick={() => setTab(t)}
              className="chip min-h-10 px-3 text-xs sm:min-h-8"
              data-testid={`golive-tab-${t}`}
            >
              {t === "live" ? tr("golive_tab_live", undefined, "Go live") : "OBS"}
            </button>
          ))}
        </div>
      )}
      {tab === "obs" && obs ? (
        <ObsTab />
      ) : loadError ? (
        <p className="text-sm text-[var(--color-warn)]">
          {loadError === "http_404"
            ? tr("golive_old_engine", undefined, "This engine can't go live: update PS5Upload on the computer it runs on.")
            : tr("golive_no_engine", undefined, "Can't reach the engine.")}
        </p>
      ) : status ? (
        <GoLiveTab status={status} />
      ) : (
        <p className="text-sm text-[var(--color-muted)]">{tr("golive_loading", undefined, "Loading…")}</p>
      )}
    </div>
  );
}

/** Keeps the status fresh while Remote Play runs: every second with the panel open, every
 *  2 s while live (the chip on the picture), else every 5 s (Go live may have been started
 *  from another window or the web UI). */
export function useGoLivePolling(active: boolean, open: boolean) {
  const refresh = useGoLiveStore((s) => s.refresh);
  const live = useGoLiveStore((s) => isLive(s.status?.live));
  useEffect(() => {
    if (!active && !live) return;
    void refresh();
    const t = window.setInterval(() => void refresh(), open ? 1000 : live ? 2000 : 5000);
    return () => window.clearInterval(t);
  }, [active, live, open, refresh]);
}

/** The Stream button's icon: a red dot on it while live. */
export function GoLiveBarIcon() {
  const live = useGoLiveStore((s) => isLive(s.status?.live));
  return (
    <span className="relative inline-flex">
      <Radio size={16} />
      <span
        aria-hidden
        className={`absolute -end-1 -top-1 h-2 w-2 rounded-full bg-[var(--color-danger,#dc2626)] ${live ? "animate-pulse" : "opacity-80"}`}
      />
    </span>
  );
}
