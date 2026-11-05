import { useCallback, useEffect, useState } from "react";
import { useNavigate } from "react-router";
import { KeyRound, LogIn, Undo2 } from "lucide-react";

import {
  payloadsDownload,
  payloadsRelease,
  profileNpBackup,
  profileNpRestore,
  profileNpState,
  sendPayloadChecked,
  type NpState,
  type ProfileInfo,
} from "../../api/ps5";
import { payloadSendRefusalText } from "../../lib/payloadSendRefusal";
import { Button, Card, ErrorCard, SuccessCard } from "../../components";
import { useConfirm } from "../../components/ConfirmDialog";
import { hostOf } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import {
  NP_FAKE_SIGNIN_ID,
  npBackupTarget,
  npSignInReadiness,
  npUndo,
} from "./npSignIn";
import { humanizeCaught } from "../../lib/humanizeError";

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
  // np-fake-signin can leave a console asking to "Temporarily Disable PS5 Console
  // Restrictions" before games open (a Discord report; upstream issue #1), and its own undo
  // (Sign out) can be greyed out. So the helper keeps a copy of everything it overwrites
  // before it runs, and Undo puts that copy back.
  const target = npBackupTarget(info);
  const [npState, setNpState] = useState<NpState | null>(null);
  const refreshNpState = useCallback(async () => {
    if (!target) {
      setNpState(null);
      return;
    }
    try {
      setNpState(await profileNpState(target.slot, target.uid, addr));
    } catch {
      // An older helper has no such method: nothing to offer, the sign-in section still works.
      setNpState(null);
    }
  }, [addr, target?.slot, target?.uid]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    void refreshNpState();
  }, [refreshNpState]);
  const undo = npUndo(npState);

  async function undoSignIn() {
    if (!target) return;
    const ok = await confirm({
      title: tr(
        "profile.np.undo_confirm_title",
        undefined,
        "Undo the sign-in?",
      ),
      message: tr(
        "profile.np.undo_confirm_body",
        { user: target.username },
        "Puts {user}'s settings and files back as they were before np-fake-signin ran. Restart the PS5 afterwards. Features that need a signed-in account, Remote Play among them, may stop opening until you sign in again.",
      ),
      confirmLabel: tr("profile.np.undo", undefined, "Undo sign-in"),
    });
    if (!ok) return;
    setError(null);
    setDone(null);
    setBusy(tr("profile.np.undoing", undefined, "Putting the settings back…"));
    try {
      const r = await profileNpRestore(target.uid, addr);
      setDone(
        tr(
          "profile.np.undone",
          { count: r.written },
          "Put back ({count} settings and files). Restart the PS5 to finish.",
        ),
      );
      await refreshNpState();
    } catch (e) {
      setError(humanizeCaught(e));
    } finally {
      setBusy(null);
    }
  }

  async function signIn() {
    if (readiness.kind !== "ready") return;
    const ok = await confirm({
      title: tr("profile.np.confirm_title", undefined, "Sign this account in?"),
      message: tr(
        "profile.np.confirm_body_v2",
        { user: readiness.username },
        "np-fake-signin writes PlayStation Network sign-in files and settings for {user} on the PS5. Nothing is sent to Sony. Restart the PS5 afterwards.",
      )
        .concat("\n\n")
        .concat(
          tr(
            "profile.np.side_effect",
            undefined,
            'Known side effect: afterwards some consoles ask to "Temporarily Disable PS5 Console Restrictions" before any game opens, and Settings > Users and Accounts > Other > Sign out can stay greyed out. The app first keeps a copy of everything np-fake-signin changes on the PS5, so Undo sign-in here puts it back.',
          ),
        ),
      confirmLabel: tr("profile.np.sign_in", undefined, "Sign in"),
    });
    if (!ok) return;
    setError(null);
    setDone(null);
    try {
      // The copy comes first: without it there is no way back, so no copy, no sign-in.
      setBusy(
        tr(
          "profile.np.backing_up",
          undefined,
          "Keeping a copy of the account's current settings on the PS5…",
        ),
      );
      try {
        // The same account and slot Undo and the state check use: the foreground uid often
        // reads 0 on 13.60, and npBackupTarget falls back to the only local user then.
        if (!target) throw new Error("np_backup_no_account");
        await profileNpBackup(target.slot, target.uid, addr);
      } catch (e) {
        const wrapped = new Error(
          tr(
            "profile.np.backup_failed",
            { error: humanizeCaught(e) },
            "The PS5 could not keep a copy of the account's settings, so np-fake-signin was not run. Replace the helper with this app's version and try again. ({error})",
          ),
        );
        // The target is ES2020, so `cause` is assigned, not passed to the constructor.
        (wrapped as Error & { cause?: unknown }).cause = e;
        throw wrapped;
      }
      setBusy(tr("profile.np.fetching", undefined, "Getting np-fake-signin…"));
      const rel = await payloadsRelease(NP_FAKE_SIGNIN_ID);
      const local = await payloadsDownload(
        NP_FAKE_SIGNIN_ID,
        rel.picked_asset_url,
        rel.tag,
      );
      setBusy(tr("profile.np.sending", undefined, "Sending it to the PS5…"));
      await sendPayloadChecked(hostOf(addr), local.path);
      setDone(
        tr(
          "profile.np.sent",
          { version: rel.tag },
          "np-fake-signin {version} ran on the PS5. Restart the PS5 to finish; the account then shows as signed in, and Settings > System > Remote Play can be turned on.",
        ),
      );
      await refreshNpState();
    } catch (e) {
      setError(payloadSendRefusalText(e));
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
      <ol className="mb-3 list-decimal space-y-1 ps-5 text-xs text-[var(--color-muted)]">
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
            "profile.np.step_restart_v2",
            undefined,
            "Restart the PS5. To undo it later, press Undo sign-in here and restart again.",
          )}
        </li>
      </ol>

      <div
        className={`mb-3 rounded-[var(--radius-field)] border px-3 py-2 text-xs ${
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

      {undo === "no_backup" && (
        <div
          className="mb-3 rounded-[var(--radius-field)] border border-[var(--color-warn)]/40 px-3 py-2 text-xs"
          data-testid="profile-np-no-backup"
        >
          {tr(
            "profile.np.no_backup",
            undefined,
            "This account carries np-fake-signin's template settings (birth date 1983-04-12, the offline-adult flag off), and no copy from before is kept on this PS5, so the app cannot undo it. If the PS5 now asks to temporarily disable console restrictions before games open: Settings > Family and Parental Controls > PS5 Console Restrictions (passcode 0000 unless you changed it), and lift the restrictions there.",
          )}
        </div>
      )}

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
        {/* Engine-only, so the web UI offers it too. */}
        {undo === "restore" && (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Undo2 size={14} />}
            loading={!!busy}
            disabled={!!busy}
            onClick={() => void undoSignIn()}
            title={tr(
              "profile.np.undo_hint",
              undefined,
              "A copy of this account's settings from before the sign-in is kept on the PS5; this puts it back.",
            )}
            data-testid="profile-np-undo"
          >
            {tr("profile.np.undo", undefined, "Undo sign-in")}
          </Button>
        )}
        {/* Payloads is desktop-only: in the web UI this button led to the Connection screen. */}
        {desktop && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => navigate("/payloads")}
          >
            {tr("remotePlay_np_open_payloads", undefined, "Open Payloads")}
          </Button>
        )}
      </div>
    </Card>
  );
}
