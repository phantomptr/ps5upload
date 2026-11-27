import { useNavigate } from "react-router";

import { Toggle } from "../../components";
import { useTr } from "../../state/lang";
import { useIsTv, useTvModeStore, useTvModeWanted } from "../../state/tvMode";

/** Settings › App › TV mode (#976): on by itself on a TV, a switch everywhere. Turning it on goes
 *  straight to TV mode's home. */
export function TvModeSetting() {
  const tr = useTr();
  const navigate = useNavigate();
  const tv = useIsTv();
  const wanted = useTvModeWanted();
  const setPref = useTvModeStore((s) => s.setPref);
  return (
    <div data-testid="settings-tv-mode">
      <Toggle
        checked={wanted}
        onChange={(on) => {
          // Back to "by itself" when the choice is what this device does anyway.
          setPref(on === tv ? null : on);
          if (on) navigate("/tv");
        }}
        label={tr("tv_mode", undefined, "TV mode")}
        hint={
          tv
            ? tr(
                "tv_mode_hint_tv",
                undefined,
                "A simple screen for playing with a remote or a controller: your PS5 and a big Play button. On by itself on this TV. Full app is one button away.",
              )
            : tr(
                "tv_mode_hint",
                undefined,
                "A simple screen for playing with a remote or a controller: your PS5 and a big Play button. Turns on by itself on a TV. Full app is one button away.",
              )
        }
      />
    </div>
  );
}
