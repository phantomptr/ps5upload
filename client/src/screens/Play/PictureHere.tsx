// The picture on this device (#670, #649): off, the stream keeps running for friends and the
// broadcast, and this device decodes, draws and plays nothing.

import { MonitorOff } from "lucide-react";

import { Button, Callout, Toggle } from "../../components";
import { useTr } from "../../state/lang";
import { useRemotePlayStore } from "../../state/remotePlay";

/** Over the stage while the picture is off here: says so, and brings it back in one click. */
export function PictureOffPlaceholder() {
  const tr = useTr();
  const setPictureHere = useRemotePlayStore((s) => s.setPictureHere);
  return (
    <div
      className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-3 bg-neutral-900 px-6 text-center text-sm text-white/85"
      data-testid="rp-picture-off"
    >
      <MonitorOff size={28} className="text-white/60" aria-hidden />
      <p className="font-medium">{tr("rp_picture_off_title", undefined, "The picture is off on this device")}</p>
      <p className="max-w-md text-xs text-white/60">
        {tr(
          "rp_picture_off_body",
          undefined,
          "The stream keeps running for your friends and your broadcast. There is no sound or controller here either, so this device has almost nothing to do.",
        )}
      </p>
      <Button
        size="sm"
        variant="primary"
        onClick={(e) => {
          e.stopPropagation();
          setPictureHere(true);
        }}
        data-testid="rp-picture-on"
      >
        {tr("rp_picture_turn_on", undefined, "Turn the picture on")}
      </Button>
    </div>
  );
}

/** The switch where Go live and Play Together are started, for this session. */
export function PictureHereToggle({ hint }: { hint: string }) {
  const tr = useTr();
  const on = useRemotePlayStore((s) => s.pictureHere);
  const setPictureHere = useRemotePlayStore((s) => s.setPictureHere);
  return (
    <div data-testid="rp-picture-toggle">
      <Toggle
        checked={on}
        onChange={setPictureHere}
        label={tr("rp_picture_here", undefined, "Picture on this device")}
        hint={hint}
      />
    </div>
  );
}

/** Hosting with the picture off: how to play on the TV. What the console does with users is its
 *  own: reported on console in #649, and nothing the app can change. */
export function PlayOnTvNote() {
  const tr = useTr();
  return (
    <div data-testid="pt-tv-note">
      <Callout tone="info" title={tr("pt_tv_title", undefined, "Playing on your TV?")}>
        {tr(
          "pt_tv_note",
          undefined,
          "This stream is signed in as the PS5 user you paired with PS5Upload. Press PS on the console's DualSense and pick a different user: that is you on the TV. Picking the stream's own user ends the stream for everyone, friends included. Some games count the stream's user as a player too, which can leave one place less for a friend.",
        )}
        <p className="mt-2" data-testid="pt-tv-seats">
          {tr(
            "pt_tv_seats",
            undefined,
            "A PS5 takes 4 controllers. With your DualSense on the TV, the first friend uses the stream's own controller and plays as its user, with no user to pick, and 2 more get a controller of their own: room for 3 friends.",
          )}
        </p>
      </Callout>
    </div>
  );
}
