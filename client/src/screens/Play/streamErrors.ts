// A stream's errors in the user's language. Apart from the Play screen, so the app shell can say a
// stream stopped while another screen shows.

import type { StreamError } from "../../lib/remotePlayStream";
import type { useTr } from "../../state/lang";

type Tr = ReturnType<typeof useTr>;

/** The plain-English line for an error from the engine or the decoder. */
export function streamErrorMessage(tr: Tr, e: StreamError): string {
  switch (e.code) {
    case "busy":
      return tr(
        "rp_err_busy",
        { s: e.retryS ?? 30 },
        `The PS5 is still finishing the last Remote Play session. Try again in ${e.retryS ?? 30} s.`,
      );
    case "held":
      // Our own last session, waited out for minutes and still not let go (#720).
      return tr(
        "rp_err_held",
        undefined,
        "The PS5 still has not let go of the last Remote Play session after several minutes. Press Start to try again. If it keeps refusing, restart the PS5.",
      );
    case "in_use":
      return tr(
        "rp_err_in_use",
        undefined,
        "Another device is using Remote Play on this PS5, or the PS5 is still finishing a session. Close Remote Play on that device (the PS Remote Play app, chiaki), then press Start.",
      );
    case "console_ended":
      return tr("rp_err_console_ended", { reason: e.detail }, `The PS5 ended the stream: ${e.detail}`);
    case "no_session":
      return tr("rp_err_no_session", undefined, "The stream ended while this page was away. Press Start to play again.");
    case "asleep":
    case "wake_failed":
      return tr(
        "rp_err_wake",
        undefined,
        "The PS5 is in rest mode and did not wake. Check that it is plugged in and that Settings › System › Power Saving › Features Available in Rest Mode has Stay Connected to the Internet and Enable Turning On PS5 from Network on.",
      );
    case "unreachable":
      return tr(
        "rp_err_unreachable",
        undefined,
        "The PS5 does not answer on the network. Check that it is on, on the same network, and that Remote Play is enabled in its settings.",
      );
    case "keys":
      return tr(
        "rp_err_keys2",
        undefined,
        "The PS5 did not accept this app's pairing. Pair again: Play › Set up › Pair.",
      );
    case "refused":
      return tr(
        "rp_err_refused",
        undefined,
        "The PS5 refused to start Remote Play. On the console, check Settings › System › Remote Play › Enable Remote Play (turn it off and on again), press Allow in Play › Set up, or pair again. The code below says which refusal it was. Still refused? Restart the PS5: a console on for a long time can refuse until it restarts.",
      );
    case "rp_off":
      return tr(
        "rp_err_rp_off",
        undefined,
        "Remote Play is switched off on the PS5. On the console, open Settings › System › Remote Play and turn on Enable Remote Play (if it already shows on, turn it off and on again), then press Start.",
      );
    case "passcode_cancelled":
      return tr("rp_err_passcode_cancelled", undefined, "Start cancelled: the PS5 needs this user's passcode to start Remote Play.");
    case "passcode_too_many":
      return tr("rp_err_passcode_too_many", undefined, "Too many wrong passcodes; enter it on the PS5.");
    case "passcode_timeout":
      return tr(
        "rp_err_passcode_timeout",
        undefined,
        "The PS5 stopped waiting for the passcode. Press Start and enter it again.",
      );
    case "no_session_id":
      // The console took the session and then never sent its session id (#720: right after a
      // restart, a game another PS5 user started was running).
      return tr(
        "rp_err_no_session_id",
        undefined,
        "The PS5 accepted the stream but did not finish starting it. Look at the TV in case the PS5 is asking something. A game started by another PS5 user can also hold it up: switch to your user or close that game, then press Start.",
      );
    case "video_refused":
      return tr("rp_err_video", undefined, "The PS5 refused that picture size. Try 720p.");
    case "decode":
      return tr("rp_err_decode", undefined, "This device could not decode the picture.");
    case "no_decoder":
      return tr("rp_err_no_decoder", undefined, "This device has no H.264 video decoder, so Remote Play cannot show the picture.");
    case "busy_other":
      return tr(
        "rp_err_busy_other",
        { host: e.host ?? "" },
        `Remote Play is already streaming ${e.host ?? "another console"}. Stop that stream first.`,
      );
    case "lost":
      return tr(
        "rp_err_lost",
        undefined,
        "The connection to the PS5 dropped during the stream. Start it again; if it keeps happening, connect the PS5 or this device by cable.",
      );
    case "socket":
      return tr(
        "rp_err_socket_lost",
        undefined,
        "Lost the connection to the engine and could not get it back within 30 s.",
      );
    default:
      return tr("rp_err_other", undefined, "The stream stopped.");
  }
}
