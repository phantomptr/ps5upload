// Failures the Rust shell reports as a code and the facts that go with it, rather than an English
// sentence (src-tauri commands/shell_error.rs). This is where they become words, in the user's
// language. The log and a bug report keep the code and facts, which say more than the sentence.

type Vars = Record<string, string | number>;

/** The translator shape both `useTr()` and the stores' non-hook helpers have. */
type Tr = (key: string, vars: Vars | undefined, fallback: string) => string;

interface Coded extends Record<string, unknown> {
  code: string;
}

/** The coded failure in an error string, or null for any other text. */
function parse(raw: string): Coded | null {
  if (!raw.startsWith('{"')) return null;
  try {
    const v = JSON.parse(raw) as unknown;
    if (v && typeof v === "object" && typeof (v as Coded).code === "string") return v as Coded;
  } catch {
    /* Not ours: a message that happens to start like JSON. */
  }
  return null;
}

/** The code of a failure the shell coded, for the callers that act on one. */
export function shellErrorCode(raw: string): string | null {
  return parse(raw)?.code ?? null;
}

const str = (v: unknown): string => (typeof v === "string" ? v : typeof v === "number" ? String(v) : "");

/** `{name}` left in a fallback the translator did not fill (the non-hook helpers do not). */
function fill(text: string, vars: Vars): string {
  return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in vars ? String(vars[k]) : m));
}

const SIMPLE: Record<string, [key: string, fallback: string]> = {
  update_no_bundle: ["update_err_no_bundle", "This release has no download for your system."],
  update_unsafe_url: [
    "update_err_unsafe_url",
    "The update address isn't one PS5Upload trusts, so nothing was downloaded.",
  ],
  update_bad_name: ["update_err_bad_name", "The update's file name isn't valid, so nothing was downloaded."],
  update_bad_checksum: [
    "update_err_bad_checksum",
    "The release lists a broken checksum for this file, so nothing was downloaded.",
  ],
  update_no_checksum: [
    "update_err_no_checksum",
    "This release lists no checksum for this file, so nothing was downloaded.",
  ],
  update_too_large: ["update_err_too_large", "The file is too large to be a PS5Upload release, so it wasn't downloaded."],
  update_interrupted: [
    "update_err_interrupted",
    "The connection was lost. Try again: the download carries on where it stopped.",
  ],
  update_checksum: [
    "update_err_checksum",
    "The download doesn't match this release's checksum. It was deleted and nothing was installed. Try again.",
  ],
  update_no_manifest: ["update_err_no_manifest", "No release with update information was found."],
  update_bad_manifest: ["update_err_bad_manifest", "The update information couldn't be read. Try again later."],
  update_busy: ["update_err_busy", "A download is already running."],
  update_not_apk: ["update_err_not_apk", "The download isn't an APK, so it was deleted. Try again."],
  update_cancelled: ["update_err_cancelled", "The download was cancelled."],
  update_cannot_self_install: [
    "update_err_cannot_self_install",
    "This copy can't install updates by itself. Install the download by hand.",
  ],
  keep_awake_unsupported: ["keep_awake_unsupported", "Not supported on this OS yet."],
  screenshot_unsupported: ["screenshot_err_unsupported", "Screenshot conversion isn't available on this device."],
  screenshot_decoder_timeout: [
    "screenshot_err_timeout",
    "This screenshot took too long to decode, so it was stopped. The file may be damaged.",
  ],
  screenshot_decoder_crashed: [
    "screenshot_err_crashed",
    "This screenshot couldn't be decoded. The file may be damaged.",
  ],
};

const ENGINE_HINTS: Record<string, [key: string, fallback: string]> = {
  windows: [
    "engine_err_hint_windows",
    "Common causes: SmartScreen or an antivirus blocked the engine program, a firewall blocks this computer's own connections, or no port was free.",
  ],
  macos: [
    "engine_err_hint_macos",
    "Common cause: security or firewall software (Little Snitch, LuLu, an antivirus with web protection) blocks ps5upload-engine. Allow it and restart the app.",
  ],
  other: [
    "engine_err_hint_other",
    "Common causes: a firewall or security tool blocks this computer's own connections to ps5upload-engine, or no port was free.",
  ],
};

function codedText(e: Coded, tr: Tr): string | null {
  const say = (key: string, vars: Vars | undefined, fallback: string) =>
    vars ? fill(tr(key, vars, fallback), vars) : tr(key, undefined, fallback);
  const simple = SIMPLE[e.code];
  if (simple) return say(simple[0], undefined, simple[1]);
  switch (e.code) {
    case "update_http":
      return say(
        "update_err_http",
        { status: str(e.status) },
        "The update server answered with error {status}. Try again later.",
      );
    case "update_rate_limited": {
      const at = typeof e.reset_at === "number" ? e.reset_at : 0;
      return at > 0
        ? say(
            "update_err_rate_limited",
            { time: new Date(at * 1000).toLocaleTimeString() },
            "GitHub is limiting requests from your network. Try again after {time}.",
          )
        : say(
            "update_err_rate_limited_later",
            undefined,
            "GitHub is limiting requests from your network. Try again in a while.",
          );
    }
    case "update_network":
      return say("update_err_network", { detail: str(e.detail) }, "Couldn't reach the update server: {detail}");
    case "update_disk":
      return say("update_err_disk", { detail: str(e.detail) }, "Couldn't save the download: {detail}");
    case "engine_not_ready": {
      const hint = ENGINE_HINTS[str(e.hint)] ?? ENGINE_HINTS.other;
      return [
        say(
          "engine_err_not_ready",
          { seconds: str(e.seconds) },
          "The engine didn't start within {seconds} seconds.",
        ),
        say(hint[0], undefined, hint[1]),
        say("engine_err_details", { error: str(e.error), log: str(e.log) }, "Last error: {error}. Log: {log}"),
      ].join(" ");
    }
    // The engine inside a phone app (src-tauri engine_in_app.rs): it did not start, or it runs
    // and the device refused the app's own connection to it (#950).
    case "engine_in_app_failed":
      return say(
        "engine_err_in_app_failed",
        { detail: str(e.detail) },
        "The engine inside the app didn't start: {detail}",
      );
    case "engine_in_app_blocked":
      return say(
        "engine_err_in_app_blocked",
        { detail: str(e.detail) },
        "The engine inside the app is running, but this device blocked the app's connection to it ({detail}). A VPN, ad blocker or content filter can do this: turn it off, then press Try again. Restarting the device can help too.",
      );
    case "engine_remote_unreachable":
      return say(
        "engine_err_remote",
        { url: str(e.url), seconds: str(e.seconds) },
        "The engine at {url} didn't answer within {seconds} seconds. Check that it is running and reachable.",
      );
    case "engine_exited_at_start":
      return e.detail
        ? say(
            "engine_err_exited_status",
            { detail: str(e.detail), log: str(e.log) },
            "The engine stopped while starting ({detail}). Log: {log}",
          )
        : say("engine_err_exited", { log: str(e.log) }, "The engine stopped while starting. Log: {log}");
    default:
      return null;
  }
}

/** Windows refusing to run the engine program at all (#914): Smart App Control or an app-control
 *  policy (os error 4551), a group policy (1260), or antivirus (225). The release isn't
 *  code-signed yet, and the generic "SmartScreen or an antivirus" hint sent people looking in the
 *  wrong place. The RAR helper's refusal is worded by the engine, so it is left alone here. */
export function windowsBlock(raw: string): "app_control" | "virus" | null {
  if (raw.includes("ps5upload-unrar")) return null;
  if (/os error (4551|1260)\b|Application Control policy|blocked by group policy/i.test(raw)) {
    return "app_control";
  }
  if (/os error 225\b|contains a virus/i.test(raw)) return "virus";
  return null;
}

const WINDOWS_BLOCK: Record<"app_control" | "virus", [key: string, fallback: string]> = {
  app_control: [
    "engine_err_app_control",
    "Windows blocked PS5Upload's engine: Smart App Control or an app-control policy stops programs that aren't code-signed, and this build isn't signed yet. On a work or school PC, ask your administrator to allow the PS5Upload folder. Otherwise you can turn Smart App Control off in Windows Security > App & browser control (once off, it can only be turned back on by resetting Windows), or run PS5Upload's Docker version on another computer and use it from a browser.",
  ],
  virus: [
    "engine_err_virus",
    "Windows Security or your antivirus stopped PS5Upload's engine as a threat. Builds that aren't code-signed are sometimes flagged by mistake. Restore ps5upload-engine from quarantine or allow it, then restart the app.",
  ],
};

/** What to show for an error string from a shell command: its words when the shell coded it, and
 *  any other text as it is. */
export function shellErrorText(raw: string, tr: Tr): string {
  const coded = parse(raw);
  const worded = coded && codedText(coded, tr);
  if (worded) return worded;
  const block = windowsBlock(raw);
  if (block) {
    const [key, fallback] = WINDOWS_BLOCK[block];
    return `${tr(key, undefined, fallback)} (${raw})`;
  }
  return raw;
}
