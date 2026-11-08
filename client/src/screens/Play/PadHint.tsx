import { eightBitDoHow } from "../../lib/padMappings";
import { useTr } from "../../state/lang";

/** One line for an 8BitDo pad in a mode the browser does not lay out the standard way (D-input,
 *  some macOS modes): X-input plays best, and how to switch to it on this model (8BitDo's
 *  manuals). A model without X-input is pointed at the check instead. */
export function EightBitDoHint({ id, className }: { id: string; className?: string }) {
  const tr = useTr();
  const kind = eightBitDoHow(id);
  if (kind === "none") {
    return (
      <span className={className} data-testid="pad-8bitdo-hint" data-how={kind}>
        {tr("pad_8bitdo_no_xinput", undefined, "This 8BitDo has no X-input mode. If a button is wrong, check the controller.")}
      </span>
    );
  }
  const how =
    kind === "switch"
      ? tr("pad_8bitdo_how_switch", undefined, "set the switch on the back to X, then press Start")
      : kind === "startx"
        ? tr("pad_8bitdo_how_startx", undefined, "turn it off, then hold Start + X to turn it on")
        : kind === "receiver"
          ? tr("pad_8bitdo_how_receiver", undefined, "use its 2.4G receiver or a USB cable")
          : tr("pad_8bitdo_how_manual", undefined, "its manual says how");
  return (
    <span className={className} data-testid="pad-8bitdo-hint" data-how={kind}>
      {tr("pad_8bitdo_xinput", { how }, `For the best result, switch your 8BitDo to X-input mode: ${how}.`)}
    </span>
  );
}
