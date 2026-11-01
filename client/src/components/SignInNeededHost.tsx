import { LogIn, UserRound } from "lucide-react";

import { Button } from "./Button";
import { Modal } from "./Modal";
import { dismissSignInNeeded, useSignInNeededStore } from "../lib/signInNeeded";
import { wakeCredentialsFor } from "../lib/useConsoleWake";
import { wakeAndSignInAsking } from "../lib/wakeSignIn";
import { useTr } from "../state/lang";
import { useRosterStore } from "../state/roster";

/** What the message says, and its buttons: Sign in only when this console has its Remote Play
 *  keys (Wake & sign in), else "sign in on the PS5". */
export function SignInNeededBody({ canSignIn }: { canSignIn: boolean }) {
  const tr = useTr();
  return (
    <p className="text-sm text-[var(--color-muted)]">
      {canSignIn
        ? tr("signin_needed_body", undefined, "This needs a signed-in user. Sign in from here, then try again.")
        : tr("signin_needed_body_manual", undefined, "This needs a signed-in user. Sign in on the PS5, then try again.")}
    </p>
  );
}

export function SignInNeededActions({
  canSignIn,
  onSignIn,
  onClose,
}: {
  canSignIn: boolean;
  onSignIn: () => void;
  onClose: () => void;
}) {
  const tr = useTr();
  return (
    <div className="flex justify-end gap-2" data-testid="signin-needed">
      <Button variant="secondary" size="sm" onClick={onClose}>
        {tr("activity_close", undefined, "Close")}
      </Button>
      {canSignIn && (
        <Button variant="primary" size="sm" leftIcon={<LogIn size={12} />} onClick={onSignIn} data-testid="signin-needed-go">
          {tr("signin_needed_go", undefined, "Sign in")}
        </Button>
      )}
    </div>
  );
}

/** "The PS5 is waiting at sign-in", over the app while a feature that needs a signed-in user
 *  found nobody (lib/signInNeeded.ts). Sign in runs Wake & sign in, passcode box included. */
export function SignInNeededHost() {
  const tr = useTr();
  const host = useSignInNeededStore((s) => s.host);
  const profiles = useRosterStore((s) => s.profiles);
  if (!host) return null;
  const creds = wakeCredentialsFor(profiles, host);
  const signIn = () => {
    dismissSignInNeeded();
    void wakeAndSignInAsking(host, creds);
  };
  return (
    <Modal
      open
      onClose={dismissSignInNeeded}
      size="sm"
      title={tr("signin_needed_title", undefined, "The PS5 is waiting at sign-in")}
      titleIcon={<UserRound size={16} />}
      footer={<SignInNeededActions canSignIn={creds.canSignIn} onSignIn={signIn} onClose={dismissSignInNeeded} />}
    >
      <SignInNeededBody canSignIn={creds.canSignIn} />
    </Modal>
  );
}
