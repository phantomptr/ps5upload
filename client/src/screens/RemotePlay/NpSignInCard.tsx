import { Download, KeyRound } from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";

/** The build that works for this: v1.4 of earthonion's np-fake-signin, the PS5 ELF. */
export const NP_FAKE_SIGNIN_VERSION = "v1.4";
export const NP_FAKE_SIGNIN_URL =
  "https://git.etawen.dev/earthonion/np-fake-signin/releases/download/v1.4/np-fake-signin-ps5.elf";

/** How to get the console's own Remote Play switch.
 *
 * The PS5 only offers Settings → System → Remote Play to an account that is signed in to
 * PlayStation Network. np-fake-signin makes the console treat an activated local account as
 * signed in (it writes the account's NP files and registry state), which is what unlocks the
 * switch. ps5upload does not run it by itself: it changes account data, so the user sends it
 * knowingly. */
export function NpSignInCard({
  onDownload,
  onOpenPayloads,
  onOpenProfile,
}: {
  onDownload: () => void;
  onOpenPayloads: () => void;
  /** Profile runs it in one step after checking the account. */
  onOpenProfile: () => void;
}) {
  const tr = useTr();
  return (
    <div
      className="rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4"
      data-testid="np-signin-card"
    >
      <div className="mb-2 flex items-center gap-2 text-sm font-medium">
        <KeyRound size={14} />
        {tr(
          "remotePlay_np_title",
          undefined,
          "Turn on the PS5's own Remote Play setting",
        )}
      </div>
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "remotePlay_np_body",
          { version: NP_FAKE_SIGNIN_VERSION },
          "The PS5 only lets you switch Remote Play on for an account that is signed in to PlayStation Network. np-fake-signin {version} makes the console treat your account as signed in, with no real PSN sign-in. After it has run, the switch under Settings → System → Remote Play can be turned on.",
        )}
      </p>
      <p className="mt-2 text-sm text-[var(--color-text)]">
        {tr(
          "remotePlay_np_profile_hint",
          undefined,
          "The easy way: Profile checks the account and runs it for you with one button.",
        )}
      </p>
      <p className="mt-2 text-xs text-[var(--color-muted)]">
        {tr("remotePlay_np_manual", undefined, "Or by hand:")}
      </p>
      <ol className="mt-1 list-decimal space-y-1 pl-5 text-sm text-[var(--color-muted)]">
        <li>
          {tr(
            "remotePlay_np_step_download",
            { version: NP_FAKE_SIGNIN_VERSION },
            "Download np-fake-signin-ps5.elf ({version}).",
          )}
        </li>
        <li>
          {tr(
            "remotePlay_np_step_send",
            undefined,
            "Send it to the PS5 from Payloads. It runs once and exits; the account must already be activated.",
          )}
        </li>
        <li>
          {tr(
            "remotePlay_np_step_restart",
            undefined,
            "Restart the PS5, then turn Remote Play on under Settings → System → Remote Play.",
          )}
        </li>
      </ol>
      <p className="mt-2 text-xs text-[var(--color-muted)]">
        {tr(
          "remotePlay_np_undo",
          undefined,
          "To undo it: Settings → Users and Accounts → Other → Sign out.",
        )}
      </p>
      <div className="mt-3 flex flex-wrap gap-2">
        <Button
          size="sm"
          variant="primary"
          onClick={onOpenProfile}
          data-testid="np-signin-open-profile"
        >
          {tr("remotePlay_np_open_profile", undefined, "Sign in from Profile")}
        </Button>
        <Button
          size="sm"
          variant="secondary"
          leftIcon={<Download size={14} />}
          onClick={onDownload}
          data-testid="np-signin-download"
        >
          {tr(
            "remotePlay_np_download",
            { version: NP_FAKE_SIGNIN_VERSION },
            "Download {version}",
          )}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          onClick={onOpenPayloads}
          data-testid="np-signin-open-payloads"
        >
          {tr("remotePlay_np_open_payloads", undefined, "Open Payloads")}
        </Button>
      </div>
    </div>
  );
}
