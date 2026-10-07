// macOS can refuse an app's connections to devices on the local network.
//
// Since macOS 15 an app needs the user's "Local Network" permission. When it is denied (the
// prompt was dismissed, or switched off later), every connection to a LAN address fails at
// once with EHOSTUNREACH, "No route to host (os error 65)", even though the console is up and
// the same network works from another computer. That reads exactly like a dead console, so
// the Connection screen adds where to allow it. Reported from the field, not reproduced here:
// the hint says "may", and is only shown for this one error on a Mac.

/** Whether a failed probe looks like macOS blocking local-network access. `userAgent` is the
 *  webview's. Errno 65 is EHOSTUNREACH on macOS only (Linux uses 113), so both are checked. */
export function looksLikeMacLocalNetworkBlock(
  error: string | null | undefined,
  userAgent: string,
): boolean {
  if (!error || !/Macintosh|Mac OS X/i.test(userAgent)) return false;
  return /os error 65\b/i.test(error) && /no route to host/i.test(error);
}
