import { useState } from "react";
import { useNavigate } from "react-router";
import { KeyRound, LogIn } from "lucide-react";

import {
  payloadsDownload,
  payloadsRelease,
  sendPayload,
  type ProfileInfo,
} from "../../api/ps5";
import { Button, Card, ErrorCard, SuccessCard } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { hostOf } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import { NP_FAKE_SIGNIN_ID, npSignInReadiness } from "./npSignIn";

/** "Sign in to PlayStation Network (offline)": earthonion's np-fake-signin, run from here.
 *
 *  The app does not carry its own copy (the project ships no license that would allow it):
 *  it fetches the published PS5 build through the Payloads catalogue and sends it, after
 *  checking the one thing the payload checks itself, an activated account for the signed-in
 *  user. */
export function NpSignInSection({
  addr,
  info,
}: {
  addr: string;
  info: ProfileInfo | null;
}) {
  const tr = useTr();
  const navigate = useNavigate();
  const { confirm, dialog } = useConfirm();
  const [busy, setBusy] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const readiness = npSignInReadiness(info);
  const desktop = isTauriEnv();

  async function signIn() {
    if (readiness.kind !== "ready") return;
    const ok = await confirm({
      title: tr("profile.np.confirm_title", undefined, "Sign this account in?"),
      message: tr(
        "profile.np.confirm_body",
        { user: readiness.username },
        "np-fake-signin writes PlayStation Network sign-in files and settings for {user} on the PS5. Nothing is sent to Sony. Restart the PS5 afterwards. To undo it: Settings > Users and Accounts > Other > Sign out.",
      ),
      confirmLabel: tr("profile.np.sign_in", undefined, "Sign in"),
    });
    if (!ok) return;
    setError(null);
    setDone(null);
    try {
      setBusy(tr("profile.np.fetching", undefined, "Getting np-fake-signin…"));
      const rel = await payloadsRelease(NP_FAKE_SIGNIN_ID);
      const local = await payloadsDownload(
        NP_FAKE_SIGNIN_ID,
        rel.picked_asset_url,
        rel.tag,
      );
      setBusy(tr("profile.np.sending", undefined, "Sending it to the PS5…"));
      await sendPayload(hostOf(addr), local.path);
      setDone(
        tr(
          "profile.np.sent",
          { version: rel.tag },
          "np-fake-signin {version} ran on the PS5. Restart the PS5 to finish; the account then shows as signed in, and Settings > System > Remote Play can be turned on.",
        ),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Card>
      {dialog}
      <div
        className="mb-1 flex items-center gap-2"
        data-testid="profile-np-signin"
      >
        <KeyRound size={16} className="text-[var(--color-accent)]" />
        <h2 className="text-sm font-semibold">
          {tr(
            "profile.np.title",
            undefined,
            "Sign in to PlayStation Network (offline)",
          )}
        </h2>
      </div>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "profile.np.body",
          undefined,
          "Makes the PS5 treat the signed-in account as signed in to PlayStation Network, without contacting Sony. Some console features only open for a signed-in account, Remote Play among them. It uses np-fake-signin by earthonion, fetched from its project page when you press the button.",
        )}
      </p>
      <ol className="mb-3 list-decimal space-y-1 pl-5 text-xs text-[var(--color-muted)]">
        <li>
          {tr(
            "profile.np.step_activate",
            undefined,
            "The account needs an account id. A PSN-linked account has one; otherwise activate it under Accounts above (offline activation).",
          )}
        </li>
        <li>
          {tr(
            "profile.np.step_run",
            undefined,
            "Press Sign in with that user signed in on the PS5. It runs once and exits.",
          )}
        </li>
        <li>
          {tr(
            "profile.np.step_restart",
            undefined,
            "Restart the PS5. To undo it later: Settings > Users and Accounts > Other > Sign out.",
          )}
        </li>
      </ol>

      <div
        className={`mb-3 rounded-lg border px-3 py-2 text-xs ${
          readiness.kind === "ready"
            ? "border-[var(--color-good)]/40"
            : "border-[var(--color-warn)]/40"
        }`}
      >
        {readiness.kind === "loading" &&
          tr("profile.np.state_loading", undefined, "Reading the account…")}
        {readiness.kind === "no_user" &&
          tr(
            "profile.np.state_no_user",
            undefined,
            "No user is signed in on the PS5. Sign in to the profile you want, then come back.",
          )}
        {readiness.kind === "no_slot" &&
          tr(
            "profile.np.state_no_slot",
            { user: readiness.username },
            "No account above is named {user}, so np-fake-signin would stop. Give the account the same name as the signed-in user, or activate one for it.",
          )}
        {readiness.kind === "not_activated" &&
          tr(
            "profile.np.state_not_activated",
            { user: readiness.username, slot: readiness.slot },
            "{user} (account {slot}) has no account id yet. Activate it under Accounts above first.",
          )}
        {readiness.kind === "ready" &&
          tr(
            "profile.np.state_ready",
            { user: readiness.username, slot: readiness.slot },
            "Ready: {user} (account {slot}) is activated.",
          )}
      </div>

      {error && (
        <div className="mb-3">
          <ErrorCard
            title={tr("profile.np.failed", undefined, "Sign-in did not run")}
            detail={error}
          />
        </div>
      )}
      {done && (
        <div className="mb-3">
          <SuccessCard title={done} />
        </div>
      )}

      <div className="flex flex-wrap items-center gap-2">
        {desktop ? (
          <Button
            size="sm"
            variant="primary"
            leftIcon={<LogIn size={14} />}
            loading={!!busy}
            disabled={readiness.kind !== "ready" || !!busy}
            onClick={() => void signIn()}
          >
            {busy ?? tr("profile.np.sign_in", undefined, "Sign in")}
          </Button>
        ) : (
          <p className="text-xs text-[var(--color-muted)]">
            {tr(
              "profile.np.browser_v2",
              undefined,
              "Signing in needs the desktop app: it fetches np-fake-signin and sends it to the PS5, which the web UI cannot do.",
            )}
          </p>
        )}
        {/* Payloads is desktop-only: in the web UI this button led to the Connection screen. */}
        {desktop && (
          <Button size="sm" variant="ghost" onClick={() => navigate("/payloads")}>
            {tr("remotePlay_np_open_payloads", undefined, "Open Payloads")}
          </Button>
        )}
      </div>
    </Card>
  );
}
