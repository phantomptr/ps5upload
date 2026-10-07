// What the Games screen says after Mount on an image ShadowMount+ manages.
//
// ShadowMount+ owns any image in a folder it scans. Since 1.7 it does not keep such an image
// mounted: it adds the game to the PS5's home screen, and mounts the image when the game
// starts (and releases it when the game exits). The old note promised a mount under
// /mnt/shadowmnt and nothing visible followed, so people waited for a mount that was never
// going to appear. This says what will happen and what to do next.

export function smpHandoffNote(
  name: string,
  opts: { added: boolean; chosePath: boolean },
): string {
  const next =
    "It mounts the image when the game starts, so nothing shows as mounted here: start the game from the PS5, or with Play under Ready to play.";
  const head = opts.added
    ? `Handed "${name}" to ShadowMount+. It adds the game to the PS5's home screen in a moment (watch the PS5 for its message).`
    : `"${name}" is already managed by ShadowMount+, so there is nothing to do here.`;
  const ignored = opts.chosePath
    ? " Your chosen mount point wasn't used: ShadowMount+ owns this image while it's in a folder ShadowMount+ scans. To mount it somewhere of your own, move it out of that folder first."
    : "";
  return `${head} ${next}${ignored}`;
}
