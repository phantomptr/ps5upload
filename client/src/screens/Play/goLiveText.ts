// Go live's names and errors in the user's language. Apart from the panel, so the app shell can say
// a run failed without loading it.

import type { LiveRun, PresetId } from "../../lib/goLive";
import type { ConfirmOptions } from "../../components/ConfirmDialog";
import type { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

export function presetName(tr: Tr, id: PresetId): string {
  return id === "custom" ? tr("golive_custom", undefined, "Custom server") : ({ youtube: "YouTube", twitch: "Twitch", kick: "Kick", facebook: "Facebook" } as const)[id];
}

/** An engine error code in the user's language (the engine's sentence when there is none). */
export function problemText(tr: Tr, code: string, fallback: string, site?: string): string {
  const s = site ?? tr("golive_the_site", undefined, "the site");
  switch (code) {
    case "unreachable":
      return tr("golive_err_unreachable", { site: s }, `Can't reach ${s}'s server. Check this computer's internet connection (and the server address).`);
    case "tls":
      return tr(
        "golive_err_tls",
        { site: s },
        `Can't make a secure connection to ${s}: its certificate could not be checked. Set SSL_CERT_FILE to this computer's certificate bundle, or check the server address.`,
      );
    case "rejected":
      return tr(
        "golive_err_rejected",
        { site: s },
        `${s} refused the stream. That is almost always the stream key: copy it again from ${s} (keys change when you reset them).`,
      );
    case "codec_not_allowed":
      return tr("golive_err_codec", { site: s }, `${s} doesn't take this video format. Restart Play in H.264 and go live again.`);
    case "dropped":
      return tr("golive_err_dropped", { site: s }, `The connection to ${s} dropped.`);
    case "stopped":
      return tr("golive_err_stopped", { site: s }, `Streaming to ${s} stopped.`);
    case "all_failed":
      return tr("golive_err_all_failed", undefined, "None of the sites took the stream.");
    case "ffmpeg_failed":
      return tr("golive_err_ffmpeg_failed", undefined, "The streaming helper (ffmpeg) could not start on this computer.");
    case "audio_failed":
      return tr("golive_err_audio_failed", undefined, "This device couldn't convert the sound for streaming sites, so the stream stopped.");
    case "audio_unavailable":
      return tr("golive_native_unavailable", undefined, "This device can't convert the sound for streaming sites, so going live isn't available here.");
    case "srt_unsupported":
      return tr("golive_err_srt_unsupported", undefined, "SRT needs PS5Upload on a computer. To go live from here, use an rtmp:// or rtmps:// address.");
    case "destination_missing":
    case "not_found":
      return tr("golive_err_destination_missing", undefined, "That site is gone. Add it again.");
    case "preset_unknown":
      return tr("golive_err_preset_unknown", undefined, "Pick YouTube, Twitch, Kick, Facebook or Custom.");
    case "key_lost":
      return tr("golive_err_key_lost", undefined, "A saved stream key can't be read. Remove that site and add it again.");
    case "key_missing":
      return tr("golive_err_key_missing", undefined, "Paste the stream key from the site.");
    case "key_invalid":
      return tr("golive_err_key_invalid", undefined, "That doesn't look like a stream key: paste only the key, without spaces.");
    case "server_missing":
      return tr("golive_err_server_missing", undefined, "Paste the Server URL from your Kick dashboard.");
    case "url_invalid":
      return tr("golive_err_url_custom", undefined, "Enter a server address starting with rtmp://, rtmps:// or srt://.");
    case "store":
      return tr("golive_err_store", undefined, "Your saved sites can't be read right now. Try again in a moment.");
    case "disk":
      return tr("golive_err_disk", undefined, "Couldn't save to this computer's disk. Check that it has free space, then try again.");
    case "unsupported":
      return tr("golive_ffmpeg_unsupported", undefined, "Going live isn't available on this computer's system.");
    case "unpack":
      return tr("golive_err_unpack", undefined, "The download could not be unpacked. Try again.");
    case "network":
      return tr("golive_no_engine", undefined, "Can't reach the engine.");
    case "play_ended":
      return tr("golive_err_play_ended", undefined, "Remote Play stopped, so the stream ended.");
    case "no_session":
      return tr("golive_err_no_session", undefined, "Start Remote Play first: Go live sends what it shows.");
    case "ffmpeg_missing":
      return tr("golive_err_ffmpeg_missing", undefined, "Download the streaming helper first.");
    case "no_destinations":
      return tr("golive_err_no_destinations", undefined, "Tick at least one place to stream to.");
    case "already_live":
      return tr("golive_err_already_live", undefined, "Already live.");
    case "checksum":
      return tr("golive_err_checksum", undefined, "The download was not the expected file, so it was deleted and not used. Try again later.");
    case "download":
      return tr("golive_err_download", undefined, "The download failed. Check this computer's internet connection and try again.");
    default: {
      // Not one of the engine's own refusals: "HTTP 500" from something in between.
      const http = /^http_(\d+)$/.exec(code);
      return http ? tr("golive_err_http", { status: http[1] }, `The engine refused that (HTTP ${http[1]}). Try again.`) : fallback;
    }
  }
}

/** The run's error when it just failed: it was live a moment ago. A failure seen for the first time
 *  (the page was opened after it) is old news, and shows in the panel. */
export function justFailed(wasLive: boolean, run: LiveRun | null): NonNullable<LiveRun["error"]> | null {
  return wasLive && run?.phase === "failed" ? (run.error ?? null) : null;
}

/** The site to name in a run's message: the one site it streams to, else none. */
export function runSite(tr: Tr, run: LiveRun): string | undefined {
  return run.destinations.length === 1 ? presetName(tr, run.destinations[0].preset) : undefined;
}

/** The question asked before a saved site is removed. Its stream key goes with it and can't be
 *  read back (only its last characters are ever shown), so one click must not be enough. */
export function removeConfirmOptions(tr: Tr, label: string): ConfirmOptions {
  return {
    title: tr("golive_remove", { name: label }, `Remove ${label}`),
    message: tr(
      "golive_remove_confirm",
      undefined,
      "Its saved stream key is removed too. To stream there again, add the site and paste its key again.",
    ),
    confirmLabel: tr("remove", undefined, "Remove"),
    destructive: true,
  };
}

/** Remove a saved site only once the user said yes. Returns whether it was removed. */
export async function removeAfterConfirm(
  confirm: (o: ConfirmOptions) => Promise<boolean>,
  opts: ConfirmOptions,
  remove: () => unknown,
): Promise<boolean> {
  if (!(await confirm(opts))) return false;
  await remove();
  return true;
}
