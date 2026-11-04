// macOS can refuse an app's connections to devices on the local network.
//
// Since macOS 15 an app needs the user's "Local Network" permission. When it is denied (the
// prompt was dismissed, or switched off later), every connection to a LAN address fails at
// once with EHOSTUNREACH, "No route to host (os error 65)", even though the console is up and
// the same network works from another computer. That reads exactly like a dead console, so
// the Connection screen adds where to allow it. Reported from the field, not reproduced here:
// the hint says "may", and is only shown for this one error on a Mac.
//
// iPhone and iPad have the same permission, and their user agents also say "Mac OS X" or
// "Macintosh", so they are told apart first (`isIos`, from lib/platform). Which error a denied
// permission gives on iOS has not been measured, and the app cannot read the permission, so any
// failed probe there says where the switch is.

/** Whether a failed probe looks like macOS blocking local-network access. `userAgent` is the
 *  webview's. Errno 65 is EHOSTUNREACH on macOS only (Linux uses 113), so both are checked. */
export function looksLikeMacLocalNetworkBlock(
  error: string | null | undefined,
  userAgent: string,
): boolean {
  if (!error || /iPhone|iPad|iPod/i.test(userAgent) || !/Macintosh|Mac OS X/i.test(userAgent)) {
    return false;
  }
  return /os error 65\b/i.test(error) && /no route to host/i.test(error);
}

/** Which Local Network instructions a failed probe gets, if any. */
export function localNetworkHintFor(
  error: string | null | undefined,
  userAgent: string,
  isIos: boolean,
): "ios" | "mac" | null {
  if (isIos) return "ios";
  return looksLikeMacLocalNetworkBlock(error, userAgent) ? "mac" : null;
}
