import { useEffect } from "react";

import { PasscodeDialog } from "../../components/PasscodeDialog";
import { useTr } from "../../state/lang";
import { rpInput, useRemotePlayStore } from "../../state/remotePlay";

/**
 * The PS5 user's sign-in passcode, when the console asks for it before Remote Play starts (#608).
 * Cancel ends the start; a wrong one asks again. Sent straight to the engine, never stored.
 */
export function RpPasscodeBox() {
  const ask = useRemotePlayStore((s) => s.passcode);
  if (!ask) return null;
  // Each ask is a fresh, empty box.
  return <StreamPasscode key={ask.seq} retry={ask.retry} />;
}

function StreamPasscode({ retry }: { retry: boolean }) {
  const tr = useTr();
  useEffect(() => {
    // The game gets an idle pad while the box is open.
    rpInput.paused = true;
    rpInput.held.clear();
    return () => {
      rpInput.paused = false;
    };
  }, []);
  return (
    <PasscodeDialog
      placement="stage"
      testId="rp-passcode"
      retry={retry}
      title={tr("rp_passcode_title", undefined, "Enter this PS5 user's passcode")}
      body={tr("rp_passcode_body", undefined, "The PS5 asks for it before Remote Play starts. It is sent to the console and not saved.")}
      onSubmit={(d) => useRemotePlayStore.getState().answerPasscode(d)}
      onCancel={() => useRemotePlayStore.getState().cancelPasscode()}
    />
  );
}
