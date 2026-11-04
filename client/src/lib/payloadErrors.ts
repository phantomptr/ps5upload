/**
 * Payload Library and USB stick errors, in the user's language.
 *
 * The backend words these with a stable code in front (`repo_host_is_ip: ...`; the engine's JSON
 * carries the same code in `code`, and the browser shim puts it back in front). A message with no
 * code we know is shown as it came.
 */
type Tr = (key: string, vars?: Record<string, string | number>, fallback?: string) => string;

const WORDED: Record<string, { key: string; fallback: string; detail?: boolean }> = {
  repo_host_is_ip: {
    key: "payloads_err_repo_host_ip",
    fallback: "That is an IP address. Use github.com or the domain of a Gitea or Forgejo server.",
  },
  repo_host_gitlab: {
    key: "payloads_err_repo_host_gitlab",
    fallback: "GitLab isn't supported. Use github.com or a Gitea or Forgejo server.",
  },
  custom_repos_unreadable: {
    key: "payloads_err_custom_repos_unreadable",
    fallback: "Your list of added repos can't be read, so it was left as it is. Fix or delete this file: {detail}",
    detail: true,
  },
  download_cancelled: {
    key: "payloads_err_download_cancelled",
    fallback: "Download cancelled.",
  },
  redirect_refused: {
    key: "payloads_err_redirect_refused",
    fallback: "The server sent this download to an address that isn't allowed, so it was stopped.",
  },
  invalid_checksum_file: {
    key: "payloads_err_invalid_checksum_file",
    fallback: "The checksum list must be a file name, like SHA256SUMS.",
  },
  usb_not_enough_space: {
    key: "usb_err_not_enough_space",
    fallback: "These payloads don't fit on this drive. Nothing on it was changed.",
  },
  usb_drive_gone: {
    key: "usb_err_drive_gone",
    fallback: "That drive isn't available any more. Plug it in again, then pick it from the list.",
  },
  usb_write_failed: {
    key: "usb_err_write_failed",
    fallback: "Couldn't write to the drive, so nothing on it was changed. ({detail})",
    detail: true,
  },
};

/** The codes the shim must keep in front of an engine error's text. */
export const WORDED_ERROR_CODES: ReadonlySet<string> = new Set(Object.keys(WORDED));

function messageOf(err: unknown): string {
  const raw = err instanceof Error ? err.message : String(err);
  return raw.replace(/^Error: /, "");
}

/** The code a message starts with, if we word it. */
function codeOf(message: string): string | null {
  const m = /^([a-z0-9_]+): /.exec(message);
  return m && WORDED_ERROR_CODES.has(m[1]) ? m[1] : null;
}

/** A user's own Cancel is not a failure: no error box, no notification. */
export function isDownloadCancelled(err: unknown): boolean {
  return codeOf(messageOf(err)) === "download_cancelled";
}

export function payloadErrorText(tr: Tr, err: unknown): string {
  const message = messageOf(err);
  const code = codeOf(message);
  if (!code) return message;
  const w = WORDED[code];
  const detail = message.slice(code.length + 2);
  return tr(w.key, w.detail ? { detail } : undefined, w.fallback.replace("{detail}", detail));
}

const UNKNOWN_SKIP = /^(.+): unknown_payload$/;

/** One "Skipped" line of a USB stick write. */
export function usbSkippedText(tr: Tr, line: string): string {
  const m = UNKNOWN_SKIP.exec(line);
  if (!m) return line;
  return tr("usb_skipped_unknown", { id: m[1] }, `${m[1]}: not in the payload list any more`);
}
