import { PasscodeDialog } from "./PasscodeDialog";
import { useTr } from "../state/lang";
import { cancelWakeSignIn, submitWakePasscode, useWakeSignInStore } from "../lib/wakeSignIn";

/** The passcode box for Wake & sign in (#608), over the whole app while the PS5 waits for it. */
export function WakePasscodeHost() {
  const tr = useTr();
  const ask = useWakeSignInStore((s) => s.ask);
  if (!ask) return null;
  return (
    <PasscodeDialog
      key={ask.seq}
      testId="wake-passcode"
      retry={ask.retry}
      sending={ask.sending}
      title={tr("rp_passcode_title", undefined, "Enter this PS5 user's passcode")}
      body={tr("wake_passcode_body", undefined, "The PS5 asks for it to sign in. It is sent to the console and not saved. Cancel leaves the PS5 at its sign-in screen.")}
      onSubmit={(d) => void submitWakePasscode(d)}
      onCancel={() => void cancelWakeSignIn()}
    />
  );
}
