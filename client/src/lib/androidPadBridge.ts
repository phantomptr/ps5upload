// Tells the Android app when controllers matter to the page (MainActivity, step 9 of
// scripts/release/android-postinit-patch.sh). While they do, it forwards pad input to the page
// (lib/nativePad) and treats pad keys as handled, so ○ during a game is not also Android's Back
// and PS is not its Home. The rest of the time a pad is left to Android, and no event crosses
// into the page for nothing. A no-op everywhere the bridge is absent.
//
// By reason, as lib/androidScreenWake is: a stream and a controller screen can overlap.

interface PadBridge {
  setActive(active: boolean): void;
}

const reasons = new Set<string>();

function bridge(): PadBridge | undefined {
  if (typeof window === "undefined") return undefined;
  return (window as unknown as { PS5UploadPad?: PadBridge }).PS5UploadPad;
}

export function setPadForwardReason(reason: string, active: boolean): void {
  const had = reasons.size > 0;
  if (active) reasons.add(reason);
  else reasons.delete(reason);
  const has = reasons.size > 0;
  if (had === has) return;
  try {
    bridge()?.setActive(has);
  } catch {
    // A bridge that throws is treated like one that is absent.
  }
}

/** Test hook. */
export function resetPadForwardForTest(): void {
  reasons.clear();
}
